use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

use libcgroups::common::{
    create_cgroup_manager, CgroupConfig, CgroupManager, ControllerOpt, FreezerState,
};
use libcgroups::stats::Stats;
use nix::unistd::Pid;
use oci_spec::runtime::{LinuxMemoryBuilder, LinuxResources, LinuxResourcesBuilder};

const CGROUP_PATH_FILE: &str = "cgroup_path";

/// Resolves the cgroup path from OCI configuration or defaults to "runphi/<container_id>".
pub fn resolve_cgroup_path(fc: &f2b::FrontendConfig) -> PathBuf {
    if let Some(cgroups_path) = fc.jsonconfig["linux"]["cgroupsPath"].as_str() {
        if !cgroups_path.trim().is_empty() {
            let path = Path::new(cgroups_path);
            let path = if path.is_absolute() {
                path.strip_prefix("/").unwrap_or(path)
            } else {
                path
            };
            return path.to_path_buf();
        }
    }
    PathBuf::from(format!("runphi/{}", fc.containerid))
}

/// Creates a CgroupConfig from runtime parameters.
fn build_cgroup_config(
    cgroup_path: PathBuf,
    container_id: &str,
    systemd_cgroup: bool,
) -> CgroupConfig {
    CgroupConfig {
        cgroup_path,
        systemd_cgroup,
        container_name: container_id.to_string(),
    }
}

/// Sets up the cgroup for the QEMU guest process, attaches the process PID, and applies resource limits.
pub fn setup_cgroups(
    fc: &f2b::FrontendConfig,
    ic: &f2b::ImageConfig,
    pid: u32,
) -> Result<(), Box<dyn Error>> {
    let cgroup_path = resolve_cgroup_path(fc);
    let systemd_cgroup = fc.jsonconfig["systemd_cgroup"]
        .as_bool()
        .unwrap_or(false);

    // Save the cgroup path in crundir for subsequent lifecycle commands (e.g., destroy/freeze/stats)
    let cgroup_path_file = fc.crundir.join(CGROUP_PATH_FILE);
    fs::write(&cgroup_path_file, cgroup_path.to_string_lossy().as_bytes())?;

    let config = build_cgroup_config(cgroup_path.clone(), &fc.containerid, systemd_cgroup);
    let manager = create_cgroup_manager(config)
        .map_err(|e| format!("Failed to create cgroup manager: {}", e))?;

    // Move the QEMU main PID into the cgroup. All sub-threads (vCPUs, I/O threads) inherit this cgroup.
    let nix_pid = Pid::from_raw(pid as i32);
    manager
        .add_task(nix_pid)
        .map_err(|e| format!("Failed to add task (PID {}) to cgroup: {}", pid, e))?;

    // Parse or build LinuxResources from OCI configuration and ImageConfig
    let resources: LinuxResources = if let Some(res_val) =
        fc.jsonconfig.get("linux").and_then(|l| l.get("resources"))
    {
        serde_json::from_value(res_val.clone()).unwrap_or_default()
    } else {
        LinuxResources::default()
    };

    // If ic.memory is explicitly set and no OCI memory limit was defined, inject it
    let resources = if ic.memory > 0 && resources.memory().is_none() {
        let mem = LinuxMemoryBuilder::default()
            .limit((ic.memory * 1024 * 1024) as i64)
            .build()?;
        let mut builder = LinuxResourcesBuilder::default();
        if let Some(cpu) = resources.cpu().clone() {
            builder = builder.cpu(cpu);
        }
        if let Some(pids) = resources.pids().clone() {
            builder = builder.pids(pids);
        }
        if let Some(block_io) = resources.block_io().clone() {
            builder = builder.block_io(block_io);
        }
        builder.memory(mem).build()?
    } else {
        resources
    };

    let oom_score_adj = fc.jsonconfig["process"]["oomScoreAdj"]
        .as_i64()
        .map(|v| v as i32);

    let controller_opt = ControllerOpt {
        resources: &resources,
        disable_oom_killer: false,
        oom_score_adj,
        freezer_state: None,
    };

    manager
        .apply(&controller_opt)
        .map_err(|e| format!("Failed to apply cgroup resource limits: {}", e))?;

    logging::log_message(
        logging::Level::Info,
        &format!(
            "Cgroup setup completed for container {} (PID {}) at {:?}",
            fc.containerid, pid, cgroup_path
        ),
    );

    Ok(())
}

/// Deletes the cgroup directory when the guest is destroyed.
pub fn destroy_cgroups(containerid: &str, crundir: &Path) -> Result<(), Box<dyn Error>> {
    let cgroup_path = get_stored_cgroup_path(containerid, crundir);
    let config = build_cgroup_config(cgroup_path.clone(), containerid, false);

    if let Ok(manager) = create_cgroup_manager(config) {
        if let Err(e) = manager.remove() {
            logging::log_message(
                logging::Level::Warn,
                &format!("Could not remove cgroup {:?}: {}", cgroup_path, e),
            );
        } else {
            logging::log_message(
                logging::Level::Info,
                &format!("Removed cgroup {:?}", cgroup_path),
            );
        }
    }

    Ok(())
}

/// Freezes or resumes all processes inside the cgroup.
pub fn freeze_cgroups(
    containerid: &str,
    crundir: &Path,
    freeze: bool,
) -> Result<(), Box<dyn Error>> {
    let cgroup_path = get_stored_cgroup_path(containerid, crundir);
    let config = build_cgroup_config(cgroup_path, containerid, false);

    let manager = create_cgroup_manager(config)
        .map_err(|e| format!("Failed to create cgroup manager for freeze: {}", e))?;

    let state = if freeze {
        FreezerState::Frozen
    } else {
        FreezerState::Thawed
    };

    manager
        .freeze(state)
        .map_err(|e| format!("Failed to set freezer state to {:?}: {}", state, e))?;

    Ok(())
}

/// Retrieves resource utilization statistics for the cgroup.
pub fn get_cgroup_stats(containerid: &str, crundir: &Path) -> Result<Stats, Box<dyn Error>> {
    let cgroup_path = get_stored_cgroup_path(containerid, crundir);
    let config = build_cgroup_config(cgroup_path, containerid, false);

    let manager = create_cgroup_manager(config)
        .map_err(|e| format!("Failed to create cgroup manager for stats: {}", e))?;

    manager
        .stats()
        .map_err(|e| format!("Failed to read cgroup stats: {}", e).into())
}

/// Reads the saved cgroup path from crundir or falls back to "runphi/<container_id>".
fn get_stored_cgroup_path(containerid: &str, crundir: &Path) -> PathBuf {
    let cgroup_path_file = crundir.join(CGROUP_PATH_FILE);
    if let Ok(content) = fs::read_to_string(&cgroup_path_file) {
        let trimmed = content.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }
    PathBuf::from(format!("runphi/{}", containerid))
}

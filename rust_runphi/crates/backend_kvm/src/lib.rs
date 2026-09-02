use nix::libc::PR_ENDIAN_BIG;
use nix::sched::{sched_setaffinity, CpuSet};
use nix::unistd::Pid;
use serde::Deserialize;
use serde_json::Value;
use std::error::Error;
use std::fs;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::str;
// use std::os::unix::net::UnixStream;

use std::thread::sleep;
use std::time::{Duration, Instant};

use std::io::Write;
// use std::io::Read;

#[allow(non_snake_case)]
pub mod configGenerator;
pub mod timer;

// Run an external command and return Err if it can't be spawned or
// exits non-zero. Replaces the .output().expect("Failed to execute
// command") pattern: the previous form panicked the entire runphi
// process on spawn failure and silently ignored non-zero exits.
fn run_command(cmd: &mut Command) -> Result<Output, Box<dyn Error>> {
    let prog = cmd.get_program().to_string_lossy().into_owned();
    let out = cmd
        .output()
        .map_err(|e| format!("failed to spawn {}: {}", prog, e))?;
    logging::log_message(
        logging::Level::Trace,
        &format!(
            "{} exited {:?}, stdout={:?}, stderr={:?}",
            prog,
            out.status,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        ),
    );
    if !out.status.success() {
        return Err(format!(
            "{} failed (exit {}): {}",
            prog,
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr).trim(),
        )
        .into());
    }
    Ok(out)
}

// Create the per-container logical volume and populate it with a clone
// of the container rootfs, so the docker image becomes the guest's
// persistent root filesystem: lvcreate -> mkfs.ext4 -> mount -> copy ->
// umount. On any failure after lvcreate the LV is removed again so a
// failed create leaves no leaked volume behind.
fn provision_lvm_root(
    lv: &str,
    size_mb: &str,
    rootfs: &Path,
    crundir: &Path,
) -> Result<(), Box<dyn Error>> {
    let mut comps = lv.rsplitn(3, '/');
    let lvname = comps.next().ok_or("malformed lv path")?;
    let vg = comps.next().ok_or("malformed lv path")?;

    run_command(
        Command::new("lvcreate")
            .arg("-L")
            .arg(format!("{}M", size_mb))
            .arg("-n")
            .arg(lvname)
            .arg(vg),
    )?;

    let mnt = crundir.join("mnt");
    let populate = (|| -> Result<(), Box<dyn Error>> {
        run_command(Command::new("mkfs.ext4").arg("-q").arg("-F").arg(lv))?;
        fs::create_dir_all(&mnt)?;
        run_command(Command::new("mount").arg(lv).arg(&mnt))?;

        let copied = run_command(
            Command::new("cp")
                .arg("-a")
                .arg(format!("{}/.", rootfs.display()))
                .arg(&mnt),
        );
        let unmounted = run_command(Command::new("umount").arg(&mnt));
        copied?;
        unmounted?;
        Ok(())
    })();

    if let Err(e) = populate {
        // Best-effort cleanup; the original error is what gets reported.
        let _ = Command::new("umount").arg(&mnt).output();
        let _ = Command::new("lvremove").arg("-y").arg(lv).output();
        return Err(e);
    }
    Ok(())
}

pub fn createguest(fc: &f2b::FrontendConfig, ic: &f2b::ImageConfig) -> Result<(), Box<dyn Error>> {
    let domain_xml = fc.crundir.join("domain.xml");
    let domain_name = format!("runphi-{}", fc.containerid);

    // // 1. Provisioning del disco LVM (se configurato dal task di storage)
    // let diskstate = fc.crundir.join("disk");
    // if diskstate.exists() {
    //     let state = fs::read_to_string(&diskstate)?;
    //     let mut parts = state.split_whitespace();
    //     let lv = parts.next().ok_or("malformed disk state file")?;
    //     let size_mb = parts.next().ok_or("malformed disk state file")?;
    //     provision_lvm_root(lv, size_mb, &fc.mountpoint, &fc.crundir)?;
    // }

    // // 2. Applicazione IRQ Steering sull'host (se specificato)
    // if !ic.isolcpu.is_empty() {
    //     if let Err(e) = isolation::apply_irq_steering(&ic.isolcpu) {
    //         eprintln!("Warning: impossibile applicare IRQ steering: {}", e);
    //     }
    // }

    // 3. Creazione e avvio del dominio Libvirt in stato di PAUSA (--paused)
    // 'virsh create' crea un dominio transitorio (non persistente, perfetto per i container)
    // '--paused' blocca la VM prima dell'avvio del payload, conforme alla semantica OCI 'create'
    let create_output = Command::new("virsh")
        .arg("create")
        .arg(&domain_xml)
        .arg("--paused")
        .output()?;

    if !create_output.status.success() {
        let stderr = String::from_utf8_lossy(&create_output.stderr);
        logging::log_message(logging::Level::Info, &format!("Fallito virsh: {}", stderr));
        return Err(format!("Fallita creazione dominio virsh: {}", stderr).into());
    }

    let mut qemu_pid = String::new();

    
    let search_pattern = format!("qemu-system.*{}", domain_name);

    let pgrep_out = Command::new("pgrep")
        .arg("-f")
        .arg(&search_pattern)
        .output()?;

    if pgrep_out.status.success() {
        qemu_pid = String::from_utf8_lossy(&pgrep_out.stdout)
            .lines()
            .next()
            .unwrap_or("")
            .trim()
            .to_string();

        logging::log_message(
            logging::Level::Info,
            &format!("PID trovato tramite pgrep: {}", qemu_pid),
        );
    } else {
        let stderr = String::from_utf8_lossy(&pgrep_out.stderr);
        logging::log_message(logging::Level::Info, &format!("Fallito pgrep: {}", stderr));
        return Err("pgrep failed".into());
    } 

    // NOTE(lorenzo): Start a small program which sees if qemu is killed. That is because 
    //                the real parent of qemu is libvirtd, not virsh. So when containerd tries to kill
    //                the PID written in the pidfile, the signal is sent to libvirtd, not containerd.
    //                containerd never knows if qemu got killed or no, so it leaves its state "Up"
    //                By using a watcher program, which dies if qemu is killed by a virsh destroy,
    //                containerd receives correctly the signal when the watcher dies. 
    let watcher = Command::new("sh")
        .arg("-c")
        .arg(format!(
            "while [ -d /proc/{} ]; do sleep 0.2; done",
            qemu_pid
        ))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;

    let watcher_pid = watcher.id().to_string();
    fs::write(&fc.pidfile, watcher_pid)?;

    Ok(())
}

pub fn startguest(containerid: &str, _crundir: &Path) -> Result<(), Box<dyn Error>> {
    let domain_name = format!("runphi-{}", containerid);

    let output = Command::new("virsh")
        .arg("resume")
        .arg(&domain_name)
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "Fallito resume del guest virsh '{}': {}",
            domain_name, stderr
        )
        .into());
    }

    Ok(())
}

pub fn stopguest(containerid: &str, _crundir: &Path) -> Result<(), Box<dyn Error>> {
    let domain_name = format!("runphi-{}", containerid);

    let output = Command::new("virsh")
        .arg("suspend")
        .arg(&domain_name)
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "Fallito stop/suspend del guest virsh '{}': {}",
            domain_name, stderr
        )
        .into());
    }

    Ok(())
}

pub fn destroyguest(containerid: &str, crundir: &Path) -> Result<(), Box<dyn Error>> {
    let domain_name = format!("runphi-{}", containerid);

    let output = Command::new("virsh")
        .arg("destroy")
        .arg(&domain_name)
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // NOTE(lorenzo): Ignore the warning if the guest was already down (ex. poweroff)
        if !stderr.contains("domain is not running") && !stderr.contains("Domain not found") {
            logging::log_message(
                logging::Level::Warn,
                &format!(
                    "virsh destroy per '{}' ha restituito: {}",
                    domain_name,
                    stderr.trim()
                ),
            );
        }
    }

    // // 2. Teardown dello storage LVM (se allocato per il container)
    // let diskstate = crundir.join("disk");
    // if let Ok(state) = fs::read_to_string(&diskstate) {
    //     if let Some(lv) = state.split_whitespace().next() {
    //         match run_command(Command::new("lvremove").arg("-y").arg(lv)) {
    //             Ok(_) => {
    //                 let _ = fs::remove_file(&diskstate);
    //             }
    //             Err(e) => logging::log_message(
    //                 logging::Level::Warn,
    //                 &format!("could not remove LV {}: {}", lv, e),
    //             ),
    //         }
    //     }
    // }

    // 3. Rimozione della working directory OCI
    fs::remove_dir_all(crundir).ok();

    Ok(())
}

pub fn storeinfo(fc: &f2b::FrontendConfig, ic: &f2b::ImageConfig) -> Result<(), Box<dyn Error>> {
    // bundle/pidfile are re-read with read_to_string by other commands and
    // parsed as path strings, so persist them as text rather than raw OsStr bytes.
    std::fs::write(
        fc.crundir.join("bundle"),
        fc.bundle.to_string_lossy().as_bytes(),
    )?;
    std::fs::write(
        fc.crundir.join("pidfile"),
        fc.pidfile.to_string_lossy().as_bytes(),
    )?;
    std::fs::write(fc.crundir.join("OS"), &ic.os_var)?;
    Ok(())
}

pub fn cleanup(_containerid: &str, crundir: &Path) -> Result<(), Box<dyn Error>> {
    fs::remove_dir_all(crundir).ok();
    Ok(())
}

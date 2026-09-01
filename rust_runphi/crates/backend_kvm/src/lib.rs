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
pub mod cgroups;

// Struttura per mappare esattamente i campi che ci interessano dall'output di QEMU
#[derive(Deserialize, Debug)]
struct QmpCpuInfo {
    #[serde(rename = "cpu-index")]
    cpu_index: usize,
    #[serde(rename = "thread-id")]
    thread_id: usize,
}

// Pin Qemu TID to pCPU
pub fn pin_thread_to_core(tid: usize, pcpu: usize) -> Result<(), String> {
    let mut cpuset = CpuSet::new();
    cpuset
        .set(pcpu)
        .map_err(|e| format!("Core {} non valido: {}", pcpu, e))?;

    let pid = Pid::from_raw(tid as i32);

    sched_setaffinity(pid, &cpuset)
        .map_err(|e| format!("Fallita sched_setaffinity per TID {}: {}", tid, e))?;

    Ok(())
}

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

fn send_qmp_cmd(socket: &Path, cmd: &str) -> Result<(), Box<dyn Error>> {
    // let mut stream = UnixStream::connect(socket)?;
    //
    // stream.write_all(cmd.as_bytes())?;
    // // stream.flush()?;
    //
    // let mut buffer = [0u8; 1024];
    // let n = stream.read(&mut buffer)?;
    // let response = String::from_utf8_lossy(&buffer[..n]);
    //
    // if !response.contains("{\"return\": {}}") {
    //     return Err(format!("Unexpected response from QMP: {}", response).into());
    // }
    let mut child = Command::new("socat")
        .arg("-")
        .arg(format!("UNIX-CONNECT:{}", socket.display()))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    // Invia i comandi QMP allo standard input di socat
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(cmd.as_bytes())?;
        stdin.flush()?;
    }

    let output = child.wait_with_output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("Errore socat sul socket {}: {}", socket.display(), stderr).into());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Verifica che QEMU abbia risposto positivamente
    if !stdout.contains("{\"return\": {}}") {
        return Err(format!("Risposta inattesa da QMP: {}", stdout).into());
    }

    Ok(())
}

pub fn createguest(fc: &f2b::FrontendConfig, ic: &f2b::ImageConfig) -> Result<(), Box<dyn Error>> {
    let conffile = fc.crundir.join("qemu.args");
    let qemu_log = fc.crundir.join("qemu.log");
    let qmp_socket = fc.crundir.join(format!("{}-qmp.sock", fc.containerid));

    // Provision the LVM-backed root disk if the config generator planned
    // one (disk_type=="lvm"): the state file holds "<lv_path> <size_mb>".
    // The file-backed mode needs no provisioning (the image is used in
    // place), and bare-metal / initramfs guests have no state file at all.
    let diskstate = fc.crundir.join("disk");
    if diskstate.exists() {
        let state = fs::read_to_string(&diskstate)?;
        let mut parts = state.split_whitespace();
        let lv = parts.next().ok_or("malformed disk state file")?;
        let size_mb = parts.next().ok_or("malformed disk state file")?;
        provision_lvm_root(lv, size_mb, &fc.mountpoint, &fc.crundir)?;
    }

    let args_content = fs::read_to_string(&conffile)?;

    let qemu_args: Vec<&str> = args_content
        .lines()
        .map(|line| line.trim()) // Rimuove eventuali spazi bianchi o carriage return (\r)
        .filter(|line| !line.is_empty())
        .collect();

    let log_out = fs::File::create(&qemu_log)?;
    let log_err = log_out.try_clone()?;

    let qemu_child = Command::new("qemu-system-x86_64")
        .args(&qemu_args)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log_out))
        .stderr(Stdio::from(log_err))
        .spawn()?;

    std::fs::write(&fc.pidfile, format!("{}", qemu_child.id()))?;

    // Attach QEMU process to cgroups and apply OCI resource restrictions
    if let Err(e) = cgroups::setup_cgroups(fc, ic, qemu_child.id()) {
        logging::log_message(
            logging::Level::Warn,
            &format!("Failed to setup cgroups for container {}: {}", fc.containerid, e),
        );
    }

    if ic.vcpu_pinning.len() == 0 {
        return Ok(());
    }

    // NOTE(lorenzo): If the user specified the pinning, have to
    //                - Obtain the TID associated to each cpu-index
    //                - For each TID, call sched_affinity to pin it to a pCPU

    let timeout = Duration::from_secs(3);
    let start_wait = Instant::now();

    while !qmp_socket.exists() {
        if start_wait.elapsed() > timeout {
            return Err(format!(
                "Timeout: QMP socket non trovato dopo 3s in: {}",
                qmp_socket.display()
            )
            .into());
        }
        sleep(Duration::from_millis(50));
    }

    let qmp_payload = "{\"execute\": \"qmp_capabilities\"}\n{\"execute\": \"query-cpus-fast\"}\n";

    let mut child = Command::new("socat")
        .arg("-")
        .arg(format!("UNIX-CONNECT:{}", qmp_socket.display()))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(qmp_payload.as_bytes())?;
    }

    let output = child.wait_with_output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("Errore socat durante l'avvio del guest: {}", stderr).into());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);

    let mut extracted_cpus: Vec<QmpCpuInfo> = Vec::new();

    for line in stdout.lines() {
        if line.trim().is_empty() {
            continue;
        }

        if let Ok(parsed) = serde_json::from_str::<Value>(line) {
            // NOTE(lorenzo): Search for the row which has the field return as array type
            //                ignoring the initial banner and qmp_capabilities first empty return
            if parsed.get("return").map_or(false, |r| r.is_array()) {
                match serde_json::from_value::<Vec<QmpCpuInfo>>(parsed["return"].clone()) {
                    Ok(cpus) => {
                        extracted_cpus = cpus;
                        break; // NOTE(lorenzo): TID Found, exit
                    }
                    Err(e) => {
                        return Err(format!("Errore nel parsing dell'array vCPU: {}", e).into());
                    }
                }
            }
        }
    }

    if extracted_cpus.is_empty() {
        return Err("Nessun Thread ID trovato nell'output di QMP".into());
    }

    for cpu_pin_req in &ic.vcpu_pinning {
        // NOTE(lorenzo): Use the vCPU index in vcpu_pinned to match th
        let matching_cpu = extracted_cpus
            .iter()
            .find(|c| c.cpu_index == cpu_pin_req.vcpu);

        match matching_cpu {
            Some(cpu) => {
                logging::log_message(logging::Level::Info, format!(
                    "Pinning vCPU {} (TID {}) al core fisico {}",
                    cpu.cpu_index, cpu.thread_id, cpu_pin_req.pcpu
                ).as_str());

                if let Err(e) = pin_thread_to_core(cpu.thread_id, cpu_pin_req.pcpu) {
                    eprintln!(
                        "Errore nell'applicare l'affinità alla vCPU {}: {}",
                        cpu.cpu_index, e
                    );
                }
            }
            None => {
                return Err(
                    format!("Attenzione: Richiesto pinning per vCPU {}, ma non esiste in QEMU.",
                    cpu_pin_req.vcpu).as_str().into()
                );
            }
        }
    }

    Ok(())
}

pub fn startguest(containerid: &str, crundir: &Path) -> Result<(), Box<dyn Error>> {
    let qmp_socket = crundir.join(format!("{}-qmp.sock", containerid));

    let timeout = Duration::from_secs(3);
    let start_wait = Instant::now();

    while !qmp_socket.exists() {
        if start_wait.elapsed() > timeout {
            return Err(format!(
                "Timeout: QMP socket non trovato dopo 3s in: {}",
                qmp_socket.display()
            )
            .into());
        }

        sleep(Duration::from_millis(50));
    }

    let qmp_payload = "{\"execute\": \"qmp_capabilities\"}\n{\"execute\": \"cont\"}\n";

    let mut child = Command::new("socat")
        .arg("-")
        .arg(format!("UNIX-CONNECT:{}", qmp_socket.display()))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    // Invia i comandi QMP allo standard input di socat
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(qmp_payload.as_bytes())?;
    }

    let output = child.wait_with_output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("Errore socat durante l'avvio del guest: {}", stderr).into());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Verifica che QEMU abbia risposto positivamente al comando 'cont'
    if !stdout.contains("{\"return\": {}}") {
        return Err(format!("Risposta inattesa da QMP: {}", stdout).into());
    }
    // let command = "{\"execute\": \"qmp_capabilities\"}\n{\"execute\": \"cont\"}";
    // send_qmp_cmd(&qmp_socket, &command)?;

    Ok(())
}

pub fn stopguest(containerid: &str, crundir: &Path) -> Result<(), Box<dyn Error>> {
    let qmp_socket = crundir.join(format!("{}-qmp.sock", containerid));

    if !qmp_socket.exists() {
        return Err(format!("QMP socket non trovato in: {}", qmp_socket.display()).into());
    }

    let command = "{\"execute\": \"qmp_capabilities\"}\n{\"execute\": \"stop\"}";
    send_qmp_cmd(&qmp_socket, &command)?;

    Ok(())
}

pub fn destroyguest(containerid: &str, crundir: &Path) -> Result<(), Box<dyn Error>> {
    let qmp_socket = crundir.join(format!("{}-qmp.sock", containerid));

    if qmp_socket.exists() {
        // Handshake QMP e invio del comando 'quit' per terminare QEMU
        let qmp_payload = "{\"execute\": \"qmp_capabilities\"}\n{\"execute\": \"quit\"}\n";

        let mut child = Command::new("socat")
            .arg("-")
            .arg(format!("UNIX-CONNECT:{}", qmp_socket.display()))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(qmp_payload.as_bytes())?;
        }

        let output = child.wait_with_output()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("Errore socat durante il quit del guest: {}", stderr).into());
        }
        // let command = "{\"execute\": \"qmp_capabilities\"}\n{\"execute\": \"quit\"}";
        // let _ = send_qmp_cmd(&qmp_socket, &command);
        let _ = std::fs::remove_file(&qmp_socket);
    }

    let diskstate = crundir.join("disk");
    if let Ok(state) = fs::read_to_string(&diskstate) {
        if let Some(lv) = state.split_whitespace().next() {
            match run_command(Command::new("lvremove").arg("-y").arg(lv)) {
                Ok(_) => {
                    let _ = fs::remove_file(&diskstate);
                }
                Err(e) => logging::log_message(
                    logging::Level::Warn,
                    &format!("could not remove LV {}: {}", lv, e),
                ),
            }
        }
    }

    // Clean up cgroups
    if let Err(e) = cgroups::destroy_cgroups(containerid, crundir) {
        logging::log_message(
            logging::Level::Warn,
            &format!("Failed to clean up cgroups for container {}: {}", containerid, e),
        );
    }

    fs::remove_dir_all(crundir).ok();

    Ok(())
}

pub fn storeinfo(fc: &f2b::FrontendConfig, ic: &f2b::ImageConfig) -> Result<(), Box<dyn Error>> {
    // bundle/pidfile are re-read with read_to_string by other commands and
    // parsed as path strings, so persist them as text rather than raw OsStr bytes.
    std::fs::write(fc.crundir.join("bundle"), fc.bundle.to_string_lossy().as_bytes())?;
    std::fs::write(fc.crundir.join("pidfile"), fc.pidfile.to_string_lossy().as_bytes())?;
    std::fs::write(fc.crundir.join("OS"), &ic.os_var)?;
    Ok(())
}

pub fn cleanup(_containerid: &str, crundir: &Path) -> Result<(), Box<dyn Error>> {
    fs::remove_dir_all(crundir).ok();
    Ok(())
}

use std::error::Error;
use std::path::Path;
use std::env;

use crate::configGenerator;
use f2b;

pub fn cpuconf(
    fc: &f2b::FrontendConfig,
    ic: &f2b::ImageConfig,
    c: &mut configGenerator::BackendConfig,
) -> Result<(), Box<dyn Error>> {
    let has_kvm = Path::new("/dev/kvm").exists();

    // 1. Configurazione Architettura, Macchina, Features e CPU Model
    #[cfg(target_arch = "aarch64")]
    {
        let host_arch = env::consts::ARCH;
        c.os_arch = "aarch64".to_string();
        c.os_machine = "virt".to_string();
        c.features_xml = "<gic version='3'/>".to_string();

        if has_kvm && host_arch == "aarch64" {
            c.domain_type = "kvm".to_string();
            c.cpu_xml = "<cpu mode='host-passthrough' check='none'/>".to_string();
        } else {
            c.domain_type = "qemu".to_string();
            c.cpu_xml = "<cpu mode='custom' match='exact'><model fallback='forbid'>max</model></cpu>".to_string();
        }
    }

    #[cfg(target_arch = "x86_64")]
    {
        c.os_arch = "x86_64".to_string();
        c.os_machine = "q35".to_string();
        c.features_xml = "<acpi/>\n    <apic/>".to_string();

        if has_kvm {
            c.domain_type = "kvm".to_string();
            c.cpu_xml = "<cpu mode='host-passthrough' check='none'/>".to_string();
        } else {
            c.domain_type = "qemu".to_string();
            c.cpu_xml = "<cpu mode='custom' match='exact'><model fallback='forbid'>qemu64</model></cpu>".to_string();
        }
    }

    // 2. Calcolo Quota OCI e Allocazione vCPU
    let period = fc.jsonconfig["linux"]["resources"]["cpu"]["period"]
        .as_f64()
        .unwrap_or(0.0);
    let quota = fc.jsonconfig["linux"]["resources"]["cpu"]["quota"]
        .as_f64()
        .unwrap_or(0.0);

    let oci_cpus = if period > 0.0 && quota > 0.0 {
        (quota / period).ceil() as u32
    } else {
        0
    };

    let allocated_vcpus = if ic.vcpus > 0 {
        ic.vcpus
    } else if !ic.vcpu_pinning.is_empty() {
        ic.vcpu_pinning.len() as u32
    } else if oci_cpus > 0 {
        oci_cpus
    } else {
        1
    };

    if oci_cpus > 0 && allocated_vcpus > oci_cpus {
        logging::log_message(
            logging::Level::Info,
            format!(
                "runPHI is allocating {} vCPUs, but the container has a limit of {:.1} CPUs (quota: {})",
                allocated_vcpus,
                (quota / period),
                quota
            )
            .as_str(),
        );
    }

    // Imposta il numero di vCPU per il tag <vcpu>
    c.vcpus = allocated_vcpus;

    // 3. Generazione dichiarativa del tag <cputune> per il Pinning
    if !ic.vcpu_pinning.is_empty() {
        let mut cputune = String::from("<cputune>\n");
        for pin in &ic.vcpu_pinning {
            cputune.push_str(&format!(
                "    <vcpupin vcpu='{}' cpuset='{}'/>\n",
                pin.vcpu, pin.pcpu
            ));
        }
        cputune.push_str("  </cputune>");
        c.cputune_xml = cputune;
    }

    Ok(())
}
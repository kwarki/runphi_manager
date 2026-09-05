# runPHI KVM Backend Documentation

Welcome to the documentation suite for the **KVM backend** (`backend_kvm`) in **runPHI**. 

runPHI is an OCI-compliant container runtime designed to partition hardware resources and run workloads inside isolated virtual partitions or bare-metal virtual machines while interfacing with standard container engines like Docker, containerd, and Kubernetes.

The `backend_kvm` crate enables runPHI to execute partitioned containers as hardware-virtualized guests using the Linux **KVM** (Kernel-based Virtual Machine) hypervisor managed through **Libvirt** and **`virsh`**.

---

## Documentation Index

This documentation suite is organized into three main guides:

1. **[Architecture and Lifecycle](architecture_and_lifecycle.md)**
   - Overview of the KVM backend architecture and how it fits into the runPHI ecosystem.
   - Host prerequisites, required virtualization packages, and host LVM storage setup.
   - Building runPHI with the KVM feature enabled (native x86_64 and cross-compiled aarch64).
   - The OCI container lifecycle mapped to Libvirt domain states (`create`, `start`, `kill`, `delete`, `state`).
   - The **Watcher Process** architecture (solving the containerd-libvirtd PID disconnection problem).
   - Real-Time features: CPU isolation (`isolcpu`, `nohz_full`), interrupt steering (`irq.rs`), and high-resolution TSC timers (`timer.rs`).
   - Resource sandboxing: host **cgroups management (`cgroups.rs`)** via `libcgroups`, confining the QEMU emulator, vCPUs, and I/O threads under host cgroups v1/v2.

2. **[Config Generator Modules Deep Dive](config_generator.md)**
   - Architectural breakdown of `crates/backend_kvm/src/configGenerator.rs`.
   - The `BackendConfig` struct and Libvirt Domain XML generation pipeline.
   - Submodule reference:
     - `cpu.rs`: architecture detection (`x86_64` / `aarch64`), KVM vs. QEMU TCG fallback, vCPU allocation hierarchy from OCI cgroup quotas, and `<cputune>` generation.
     - `boot.rs`: guest differentiation (Linux direct kernel boot vs. bare-metal / Zephyr unikernels) and kernel command lines.
     - `network.rs`: virtual network interfaces (User/SLIRP mode, Linux host Bridge mode, Libvirt virtual network).
     - `disk.rs`: virtual disk concepts (file-backed and LVM-backed storage), and step-by-step instructions for host LVM volume group preparation.
     - `irq.rs`: dynamic host IRQ affinity steering and automated cleanup.

3. **[Container Boot Config Reference (`/boot/config.json`)](config_json_reference.md)**
   - Complete schema and field-by-field reference for `/boot/config.json`.
   - Practical configuration templates for different scenarios:
     - Bare-metal / Zephyr RTOS inmate.
     - Standard Linux guest running from initramfs.
     - File-backed persistent disk guest.
     - Host LVM-backed persistent block storage guest.
     - Real-Time Linux guest with strict vCPU pinning, core isolation, and IRQ steering.
     - Linux guest with User-mode (SLIRP) or Bridge networking.
   - Passing host cgroup constraints via Docker (`--cpuset-cpus`, `--memory`, `--cpu-quota`).
   - Step-by-step tutorial on building and running a container image with Docker.

---

## Key Features of the KVM Backend

| Feature | Description |
|---|---|
| **Hypervisor Integration** | Uses Linux KVM hardware acceleration via Libvirt (`virsh`) and QEMU. Automatically falls back to QEMU TCG software emulation if `/dev/kvm` is absent. |
| **Multi-Architecture** | Supports both `x86_64` (machine type `q35`, APIC/ACPI) and `aarch64` (machine type `virt`, GICv3). |
| **cgroups v1/v2 Sandboxing** | Integrates with `libcgroups` to place the QEMU main process and all child threads (vCPUs, I/O workers) into dedicated host cgroups, strictly enforcing OCI CPU quotas, CPU affinity (`cpuset`), and memory limits. |
| **Real-Time CPU Pinning** | Maps virtual CPUs (`vCPU`) directly to physical host CPUs (`pCPU`) using Libvirt `<cputune>` and `<vcpupin>`. |
| **Automated IRQ Steering** | Automatically migrates host interrupt affinities (`/proc/irq/*/smp_affinity_list`) away from isolated cores to dedicated housekeeper cores, restoring them on container termination. |
| **Host LVM Storage** | Dynamically provisions, formats (`mkfs.ext4`), populates with the container rootfs, and tears down per-container host Logical Volumes (`/dev/<vg>/lv_<id>`). |
| **PID Watcher** | Bridges the gap between container managers (like containerd) and Libvirt daemon children, ensuring synchronous process termination reporting. |
| **Flexible Networking** | Offers out-of-the-box support for isolated User-mode SLIRP, host Linux bridges (`docker0`, `virbr0`), and Libvirt managed networks. |

---

## Quick Example

A runPHI container is an OCI container whose root filesystem contains `/boot/config.json`. For instance, to launch a minimal Linux guest:

```json
{
  "os_var": "linux",
  "inmate": "/boot/Image",
  "ramdisk": "/boot/rootfs.cpio.gz",
  "memory": 1024,
  "vcpus": 2,
  "net": "user"
}
```

When you run this container via `docker run --runtime=runphi <image_name>`, runPHI intercepts the call, generates a Libvirt `domain.xml`, spawns the domain via `virsh create --paused`, binds a watcher process, and then boots the virtual machine via `virsh resume` upon the OCI `start` command.

Proceed to **[Architecture and Lifecycle](architecture_and_lifecycle.md)** to learn how the backend works and how to set up your environment.

# Container Boot Config Reference (`/boot/config.json`)

This document is a practical guide for users authoring container images for runPHI using the **KVM backend**. It details the role of `/boot/config.json`, provides a complete field-by-field reference, presents concrete configuration templates, and walks through packaging a complete container image with Docker.

---

## 1. What is `/boot/config.json`?

In standard Docker or OCI containers, container engines launch processes using Linux namespaces and cgroups on the host kernel. In runPHI, a container can instead represent a **partitioned virtual machine** or **bare-metal guest**.

To distinguish between standard containers and partitioned guests, runPHI inspects the root filesystem of the container image at creation time:
- **Presence of `/boot/config.json`**: runPHI intercepts the container and handles its lifecycle using the compiled hypervisor backend (`backend_kvm`).
- **Absence of `/boot/config.json`**: runPHI transparently forwards the OCI execution to vanilla `runc` (`/usr/local/sbin/runc_vanilla`), running it as a conventional Linux container.

The `/boot/config.json` file describes the virtual machine parameters: the kernel to boot, memory size, CPU topologies, real-time isolation parameters, and virtual networking devices.

---

## 2. Field-by-Field Reference

All paths specified inside `/boot/config.json` (such as `inmate` and `ramdisk`) are specified **relative to the container's root filesystem** (e.g. `/boot/Image` resolves to `<mountpoint>/boot/Image`).

| Field | Type | Default | Description |
|---|---|---|---|
| `os_var` | String | `""` | Operating system type. Use `"linux"` for a Linux kernel + initramfs/disk guest. Use `"zephyr"` or any other name for bare-metal or unikernel binaries. |
| `inmate` | String | `"/boot/inmate"` | Path inside the container to the guest executable. For Linux guests, this is the kernel binary (e.g. `/boot/Image` or `/boot/vmlinuz`). For bare-metal, this is the raw binary or ELF file. |
| `ramdisk` | String | `""` | Path inside the container to the initial ramdisk / initramfs (e.g. `/boot/rootfs.cpio.gz`). Required for Linux guests booting with an initramfs root. |
| `dtb` | String | `""` | Optional path to an ARM Device Tree Blob (e.g. `/boot/virt.dtb`). Primarily used on `aarch64` when using custom peripheral trees. |
| `memory` | Integer | `0` (dynamic) | Guest RAM size in **Megabytes** (e.g. `1024` for 1 GB). If omitted or `0`, runPHI falls back to the OCI container memory limit (`fc.jsonconfig["linux"]["resources"]["memory"]["limit"]`); if that is also unset, it defaults to **1024 MB** for Linux and **32 MB** for bare-metal. Additionally, `cgroups.rs` enforces this bound on the host QEMU cgroup. |
| `vcpus` | Integer | `0` (dynamic) | Number of virtual CPUs to allocate. Sizing priority: explicit `vcpus` $\rightarrow$ `vcpu_pinning.len()` $\rightarrow$ OCI cgroup CPU quota $\left(\left\lceil\frac{\text{quota}}{\text{period}}\right\rceil\right)$ $\rightarrow$ default `1`. A warning is logged if `vcpus` exceeds the OCI cgroup quota. |
| `vcpu_pinning` | Array of Objects | `[]` | Explicit static mapping of virtual CPUs (`vcpu`) to host physical CPUs (`pcpu`). Example: `[{"vcpu": 0, "pcpu": 2}, {"vcpu": 1, "pcpu": 3}]`. Generates Libvirt `<cputune>` XML. |
| `isolcpu` | String | `""` | Identifies host CPUs that are isolated from the OS scheduler (e.g. `"2,3"` or `"2-5"`). Used by the IRQ steering module to prevent interrupt redirection to real-time cores. |
| `nohz_full` | String | `""` | Identifies host CPUs running in full tickless mode (adaptive ticks). Merged with `isolcpu` for safety validations. |
| `steer_irq`<br/>*(alias: `irq_steering`)* | Array of Integers | `null` | Host CPU IDs where all movable host hardware interrupts should be redirected while the container runs (e.g. `[0, 1]`). Restored automatically when the container is deleted. |
| `net` | String / Boolean | `"no"` | Network mode configuration. Accepts booleans (`true`/`false`) or strings (`"user"`, `"slirp"`, `"bridge"`, `"bridge:<name>"`, `"network:<name>"`). See details below. |
| `netconf` | String | `""` | Additional network parameter. When `net` is set to `"bridge"`, `netconf` specifies the target host bridge interface name (e.g. `"custombr0"`). |
| `disk_type` | String | `""` | Root disk strategy: `""` (initramfs in RAM), `"file"` (raw disk image), or `"lvm"` (host LVM volume). When set to `"lvm"`, runPHI provisions a host Logical Volume (`/dev/<vg>/lv_<id>`), formats with ext4, clones the rootfs into it, and boots with `root=/dev/vda rw`. |
| `disk_image` | String | `""` | Path inside the rootfs to a raw disk image when `disk_type: "file"`. |
| `disk_size` | Integer | `0` | Disk size in MB when `disk_type: "lvm"`. If `0`, runPHI auto-calculates rootfs size + 30% slack + 64 MB metadata floor. |

---

## 3. Practical Configuration Templates

### Template 1: Minimal Bare-Metal / Zephyr OS

For unikernels, Zephyr RTOS, or raw ELF inmates:

```json
{
  "os_var": "zephyr",
  "inmate": "/boot/zephyr.elf",
  "memory": 64,
  "vcpus": 1,
  "net": "no"
}
```

- Boots directly with QEMU without an initramfs or Linux command line.
- Allocates 64 MB of RAM and 1 vCPU.

---

### Template 2: Standard Linux Guest (Initramfs Root)

For standard Linux guests booting an `Image` kernel and running rootfs from a compressed initramfs in RAM:

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

- Kernel boots with `console=ttyS0,115200`.
- The initramfs is loaded into RAM as the root filesystem.
- Attaches an isolated User-mode (SLIRP) virtio network card.

---

### Template 3: File-Backed Persistent Disk

For guests running from an ext4 disk image packaged within the container:

```json
{
  "os_var": "linux",
  "inmate": "/boot/Image",
  "disk_type": "file",
  "disk_image": "/boot/rootfs.img",
  "memory": 512,
  "vcpus": 1,
  "net": "no"
}
```

- Kernel boots with `console=ttyS0,115200 root=/dev/vda rw`.
- Rootfs changes persist inside `/boot/rootfs.img` within the container storage layer.

---

### Template 4: Host LVM-Backed Persistent Block Storage

For guests requiring high-performance raw block device storage provisioned directly on the host:

```json
{
  "os_var": "linux",
  "inmate": "/boot/Image",
  "disk_type": "lvm",
  "disk_size": 2048,
  "memory": 1024,
  "vcpus": 2,
  "net": "user"
}
```

- **Host Prerequisite**: The host must have Volume Group `test-vg` active (or the VG name configured in `/usr/share/runPHI/kvm_lvm_vg`).
- **Dynamic Provisioning**: During `create`, runPHI runs `lvcreate` (2048 MB), formats with `mkfs.ext4`, mounts the volume, copies the container rootfs into it (`cp -a`), and unmounts.
- **Kernel Boot**: Boots with `console=ttyS0,115200 root=/dev/vda rw`.
- **Teardown**: During `delete`, runPHI automatically executes `lvremove -y`, ensuring no leaked volumes.

---

### Template 5: Real-Time Isolated Linux Guest

For safety-critical or low-latency workloads requiring dedicated physical CPU cores and interrupt isolation:

```json
{
  "os_var": "linux",
  "inmate": "/boot/Image",
  "ramdisk": "/boot/rootfs.cpio.gz",
  "memory": 2048,
  "vcpus": 2,
  "vcpu_pinning": [
    { "vcpu": 0, "pcpu": 2 },
    { "vcpu": 1, "pcpu": 3 }
  ],
  "isolcpu": "2,3",
  "nohz_full": "2,3",
  "steer_irq": [0, 1],
  "net": "no"
}
```

- **vCPU Pinning**: vCPU 0 is pinned to host CPU 2; vCPU 1 is pinned to host CPU 3.
- **Core Isolation**: Declares cores 2 and 3 as isolated real-time cores.
- **IRQ Steering**: Automatically redirects all host hardware interrupts (`/proc/irq/*/smp_affinity_list`) to housekeeping cores 0 and 1, protecting cores 2 and 3 from interrupt jitter. Original affinities are restored upon exit.

---

### Template 6: Combining `/boot/config.json` with Docker Host cgroup Controls

You can combine `/boot/config.json` with standard Docker resource constraints to enforce hard host cgroup limits on the hypervisor:

```bash
docker run -d \
  --runtime=runphi \
  --cpuset-cpus=2,3 \
  --memory=2048m \
  --cpu-quota=100000 \
  --cpu-period=100000 \
  my-kvm-image:latest
```

#### How Docker and runPHI Collaborate via cgroups:
1. Docker populates `fc.jsonconfig["linux"]["resources"]` with `cpuset.cpus: "2,3"`, `memory.limit: 2147483648`, and CPU quota parameters.
2. `cgroups.rs` parses these constraints into `LinuxResources` using `libcgroups`.
3. runPHI attaches the QEMU main process PID to the container cgroup (`/sys/fs/cgroup/runphi/<id>/`).
4. **Result**: The main QEMU emulator thread, event loop, and asynchronous I/O threads are confined strictly to cores 2 and 3, and cannot consume more than 2048 MB of host RAM.

---

### Template 7: Linux Guest with Host Bridge Networking

To connect the guest directly to an existing host Linux bridge (e.g. `docker0` or `virbr0`) so that other containers or external network machines can reach it:

```json
{
  "os_var": "linux",
  "inmate": "/boot/Image",
  "ramdisk": "/boot/rootfs.cpio.gz",
  "memory": 1024,
  "vcpus": 2,
  "net": "bridge:docker0"
}
```

Alternatively, using the Libvirt virtual network `default`:
```json
{
  "os_var": "linux",
  "inmate": "/boot/Image",
  "ramdisk": "/boot/rootfs.cpio.gz",
  "memory": 1024,
  "vcpus": 2,
  "net": "network:default"
}
```

---

## 4. End-to-End Packaging Tutorial: Building a Container Image

This tutorial walks through building a deployable container image that runs a Linux guest on runPHI using Docker.

### Step 1: Prepare the Guest Assets
You need:
1. An x86_64 or aarch64 kernel binary (`Image`).
2. An initramfs archive (`rootfs.cpio.gz`) containing `/init` and a minimal userland (e.g. BusyBox or Alpine Linux).
3. The `config.json` configuration file.

Create a workspace directory:
```bash
mkdir -p my_guest/boot
cd my_guest
```

Place your kernel and initramfs inside `boot/`:
```bash
cp /path/to/my/kernel/Image boot/Image
cp /path/to/my/initramfs.cpio.gz boot/rootfs.cpio.gz
```

### Step 2: Write `boot/config.json`
Create `boot/config.json`:
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

### Step 3: Write the Dockerfile
Create a `Dockerfile` in `my_guest/`:
```dockerfile
FROM scratch

# Copy the guest boot payload into /boot
COPY boot/ /boot/

# Keep a dummy entrypoint for OCI compatibility
CMD ["/bin/sh"]
```

### Step 4: Build the Image
Build the container image using standard Docker:
```bash
docker build -t runphi-linux-guest:latest .
```

### Step 5: Launch with runPHI

#### Option A: Standard Launch
Run the container using the `runphi` runtime:

```bash
docker run --rm -it --runtime=runphi runphi-linux-guest:latest
```

#### Option B: Launch with Strict Host cgroup Resource Limits
To enforce CPU affinity and RAM ceilings on the hypervisor and all helper threads:

```bash
docker run --rm -it \
  --runtime=runphi \
  --cpuset-cpus=2,3 \
  --memory=1024m \
  runphi-linux-guest:latest
```

#### Option C: Launching an LVM-Backed Persistent Guest
1. Ensure the host has Volume Group `test-vg` prepared (see [Architecture and Lifecycle](architecture_and_lifecycle.md#host-lvm-storage-prerequisites) or [Config Generator](config_generator.md#user-guide-steps-needed-on-the-host-before-runphi-can-create-the-lvm)):
   ```bash
   sudo vgs test-vg
   ```
2. In `my_guest/boot/config.json`, configure LVM storage:
   ```json
   {
     "os_var": "linux",
     "inmate": "/boot/Image",
     "disk_type": "lvm",
     "disk_size": 2048,
     "memory": 1024,
     "vcpus": 2,
     "net": "user"
   }
   ```
3. Rebuild and run:
   ```bash
   docker build -t runphi-lvm-guest:latest .
   docker run --rm -it --runtime=runphi runphi-lvm-guest:latest
   ```
4. While running, verify the provisioned volume on the host:
   ```bash
   sudo lvs test-vg
   # Shows lv_<containerid>
   ```

---

### What Happens Under the Hood:
1. Docker passes the container bundle and OCI spec to `runphi`.
2. runPHI finds `/boot/config.json` in the rootfs and activates `backend_kvm`.
3. `configGenerator` creates `/run/runPHI/<id>/domain.xml`. If `disk_type` is `"lvm"`, it plans `/run/runPHI/<id>/disk`.
4. If LVM is requested, `provision_lvm_root` invokes `lvcreate`, `mkfs.ext4`, mounts the volume, copies the container rootfs, and unmounts.
5. `virsh create /run/runPHI/<id>/domain.xml --paused` spawns the QEMU domain.
6. runPHI discovers the QEMU PID and invokes `cgroups::setup_cgroups(fc, ic, pid)`:
   - Places the QEMU main PID into `/sys/fs/cgroup/runphi/<id>/`.
   - Enforces OCI CPU quotas, `cpuset.cpus`, and memory limits across all QEMU threads.
7. runPHI starts the supervisor watcher process monitoring the QEMU PID.
8. Upon receiving the `start` command, `virsh resume runphi-<id>` unpauses the guest.
9. The guest boots!
10. When the guest halts or `docker stop` is executed:
    - `virsh destroy` terminates the domain.
    - If an LVM disk was provisioned, `lvremove -y` unlinks and deletes the logical volume.
    - `cgroups::destroy_cgroups` deletes the cgroup directory.
    - The watcher process exits, and Docker cleanly records container termination.

---

### Inspecting Logs and Runtime State
- **Global runPHI execution log**:
  ```bash
  cat /usr/share/runPHI/log.txt
  ```
- **Libvirt Domain Status**:
  ```bash
  sudo virsh list --all
  ```
- **Active Host cgroups**:
  ```bash
  # Under cgroups v2:
  cat /sys/fs/cgroup/runphi/<container_id>/cgroup.procs
  cat /sys/fs/cgroup/runphi/<container_id>/cpuset.cpus
  ```
- **Active LVM Logical Volumes**:
  ```bash
  sudo lvs
  ```

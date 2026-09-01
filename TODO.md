# TODO

1. Estendere configGenerator con moduli specifici per configurare meglio qemu
    1. boot.rs: stabilisce se inmate è linux o bare metal, abilita/disabilita KVM, configura in generale il boot di qemu
    2. cpu.rs: gestisce il numero di CPU, vCPU Pinning
    3. disk.rs: gestisce la presenza di un disk, gestione del boot dal disk (?). Creazione di un LVM se non presente
    4. mem.rs: gestisce l'allocazione di memoria per la macchina virtuale
    5. net.rs: gestisce i device network

2. Estendere config.json per abilitare tecniche di isolamento
    1. vCPU Pinning
    2. isolcpu / nohz_full
    3. IRQ Steering
    4. Memory locking
    5. cgroups 
    6. (Opzionale) memguard 

## Assegnati

Dettaglio dei Task

1. Lorenzo — CPU, Boot & Real-Time Isolation

    cpu.rs & boot.rs:

        Gestione parametri vCPU, flag KVM (-enable-kvm) e distinzione tra guest Linux vs Bare-Metal.

        Generazione argomenti di boot del kernel (-append).

    Isolamento in config.json:

        Configurazione per affinity/pinning dei core vCPU.

        Inserimento e propagazione dei parametri kernel isolcpu e nohz_full.

        Configurazione per l'IRQ affinity / steering su core dedicati.

2. Antonio — Memory Management & Process Confinement

    mem.rs:

        Allocazione memoria QEMU (-m, backend per HugePages o shared memory se necessario).

    Isolamento in config.json:

        Configurazione del memory locking (es. mlockall per evitare page faults/swapping).

        Integrazione cgroups per limitare e vincolare le risorse del processo QEMU (cpuset, memory).

        (Opzionale) Supporto alla configurazione per memguard per il throttling della banda di memoria.

3. Carmine — Storage, LVM & Network Devices

    disk.rs:

        Gestione drive/dischi virtuali e priorità di boot.

        Logica di provisioning/verifica del volume LVM (esecuzione comandi o integrazione libreria per creare LVM se non presente).

    net.rs:

        Configurazione backend di rete QEMU (tap, bridge, user/slirp) e interfacce guest (virtio-net).

    Integrazione config.json:

        Definizione dello schema JSON per la sezione dischi (path, formato, LVM target) e interfacce di rete (MAC, bridge name).

Perché questa suddivisione è bilanciata

    Carico di complessità: disk.rs include logica di sistema non banale (gestione LVM), motivo per cui è accoppiato a net.rs e non a task di isolamento pesanti.

    Dipendenze minime: I parametri kernel (isolcpu/nohz_full) vanno a braccetto con boot.rs, mentre cpu.rs condivide naturalmente la logica di pinning e IRQ steering.

    Isolamento della memoria: mem.rs è più compatto a livello di argomenti QEMU diretti, ma viene bilanciato dalla gestione dei cgroups e del memory locking.


# **BUG (?)**:

When a guest executes `poweroff` or receives a `docker stop`, Docker triggers an OCI delete.

Currently, runPHI also deletes the container's state directory (/run/runPHI/<id>).

Because the directory was prematurely destroyed, running `docker start` on the old container, runPHI's forwarding logic can no longer recognize the container, hence the forwarding to runc vanilla:
`Error: Failed to exec /usr/local/sbin/runc_vanilla: No such file or directory (os error 2)`

According to oci standard, `docker start` results in oci commands `delete` -> `create` -> `start`.

The forwarding decision checks that /run/runPHI/<id> is present.

## FIX

`docker stop` should just remove the content of `/run/runPHI/<id>/*`, but not delete the directory itself, in this way runphi can remember that he is the one handling the container.

Only `docker rm` should result in the removal of the state directory.

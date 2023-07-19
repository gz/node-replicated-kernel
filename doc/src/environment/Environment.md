# Environment

This chapter contains various notes on configuration and setup of the host
system (Linux), and the hypervisor (QEMU) to use either pass-through or emulate
various technologies for the nrkernel and develop for it.

## Install QEMU from sources

Make sure the QEMU version for the account is is >= 6 . The following steps can
be used to build it from scratch, if it the Ubuntu release has a lesser version
in the package repository.

First, make sure to uncomment all #deb-src lines in /etc/apt/sources.list if not
already uncommented. Then, run the following commands:

```bash
sudo apt update
sudo apt install build-essential libpmem-dev libdaxctl-dev ninja-build flex bison
apt source qemu
sudo apt build-dep qemu
wget https://download.qemu.org/qemu-6.0.0.tar.xz
tar xvJf qemu-6.0.0.tar.xz
cd qemu-6.0.0
./configure --enable-rdma --enable-libpmem
make -j 28
sudo make -j28 install
sudo make rdmacm-mux

# Check version (should be >=6.0.0)
qemu-system-x86_64 --version
```

You can also add `--enable-debug` to the configure script which will add debug
information (useful for source information when stepping through qemu code in
gdb).

For rackscale mode, install an updated ivshmem-server:
```bash
git clone https://github.com/hunhoffe/qemu.git qemu.tar.xz
cd qemu-6
git checkout --track origin/dev/ivshmem-numa
./configure
make -j 28
```

Use ```which ivshmem-server``` to find the current location and then overwrite it
with ```qemu/build/contrib/ivshmem-server/ivshmem-server```. 

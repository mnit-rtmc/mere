# Mere

*Mere* is a low-latency directory mirroring service using ssh.

## Build

```
cargo build --release
```

* Edit `./mere.service`
* Add {host:post} and directories to `ExecStart=/usr/local/bin/mere` line

## Installation

As root:
```
cp ./target/release/mere /usr/local/bin/
cp ./mere.service /etc/systemd/system/
systemctl enable mere
systemctl start mere
```

Check status with:
```
systemctl status mere
```

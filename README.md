# Mere

![Mere](mere.svg)

*Mere* is a low-latency directory mirroring program.

## Building

With cargo:

```
cargo build --release
```

## Running

You must specify the destination host as well as one or more directories to
mirror.

```
./target/release/mere {host} {dir 0} ... {dir N}
```

## Running as a systemd Service

As root:

```
cp ./target/release/mere /usr/local/bin/
cp ./mere.service /etc/systemd/system/
```

* Edit `/etc/systemd/system/mere.service`
* Add {host:port} and directories to `ExecStart=/usr/local/bin/mere` line

```
systemctl enable mere
systemctl start mere
```

Check status with:
```
systemctl status mere
```

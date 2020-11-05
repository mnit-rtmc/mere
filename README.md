# Mere

![Mere](mere.svg)

*Mere* is a low-latency directory mirroring program for Linux.

Authentication happens using one of two methods:

1. Public key authentication using a private key from
   `/home/{username}/.ssh/id_rsa`.  This only works if the file does not require
   a password.
2. Agent authentication, which should work when running interactively.

It has minimal runtime dependencies, using bundled versions of ssh and openssl.

## Building

With cargo:

```
cargo build --release
```

## Running

You must specify the destination host as well as one or more source directories
to mirror.

```
./target/release/mere -d {dst host} -s {dir 0} ... {dir N}
```

## Running as a systemd Service

As root:

```
cp ./target/release/mere /usr/local/bin/
cp ./mere.service /etc/systemd/system/
```

* Edit `/etc/systemd/system/mere.service`
* Replace {destination} and {sources} on `ExecStart=/usr/local/bin/mere` line

```
systemctl enable mere
systemctl start mere
```

Check status with:
```
systemctl status mere
```

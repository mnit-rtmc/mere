# Mere

![Mere](mere.svg)

*Mere* is a real-time file mirroring tool for Linux.

It has minimal runtime dependencies, using bundled versions of `libssh2` and
`openssl`.

Authentication happens using one of two methods:

1. Public key authentication using a private key from
   `/home/{username}/.ssh/id_rsa`.  This only works if the file does not require
   a password.
2. Agent authentication, which should work when running interactively.

## Building

With `cargo`:

```
cargo build --release
```

## Running

```
Usage: ./target/release/mere [OPTIONS]

A real-time file mirroring tool


Optional arguments:
  -h, --help       Print help message
  -d, --destination DESTINATION
                   Destination: <host> or <host>:<port>
  -p, --path PATH  Directory or file path (can be used multiple times)
  -w, --watch      Watch paths for changes using inotify
```

* `--destination` is required
* One or more `--path` arguments are required
* `--watch` uses inotify to watch each specified path, mirroring files which are
  **closed** after writing, **deleted** or **moved**.

## Running as a systemd Service

As root:

```
cp ./target/release/mere /usr/local/bin/
cp ./mere.service /etc/systemd/system/
```

* Edit `/etc/systemd/system/mere.service`
* Replace {destination} and {path} on `ExecStart=/usr/local/bin/mere` line

```
systemctl enable mere
systemctl start mere
```

Check status with:
```
systemctl status mere
```

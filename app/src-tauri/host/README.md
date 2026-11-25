# moonfish-host builds

The release build puts `moonfish-host-{x86_64,aarch64}-linux` (static musl builds) and
`moonfish-host-{aarch64,x86_64}-darwin` (macOS builds) of `crates/moonfish-host` here, and the app
bundles them. Moonfish copies the right one to an SSH host the first time it's used. To build them
locally:

    bun scripts/build-host.ts            # Linux on Linux, macOS on macOS
    bun scripts/build-host.ts --linux    # Linux musl (via cargo-zigbuild)
    bun scripts/build-host.ts --darwin   # macOS arm64 + x86_64 (on macOS)
    bun scripts/build-host.ts --all      # both Linux and macOS (on macOS)

Without them the app still works with SSH hosts; it starts one `ssh` per program instead.

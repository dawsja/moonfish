#!/usr/bin/env bun
// Builds moonfish-host for the SSH hosts Moonfish supports (Linux static musl, macOS Darwin) and
// puts the binaries where the desktop app bundles them:
//   app/src-tauri/host/moonfish-host-<arch>-<os>
//
//   bun scripts/build-host.ts            # Linux on non-macOS, Darwin on macOS
//   bun scripts/build-host.ts --linux    # x86_64-linux, aarch64-linux (needs cargo-zigbuild + zig)
//   bun scripts/build-host.ts --darwin   # aarch64-darwin, x86_64-darwin (must run on macOS)
//   bun scripts/build-host.ts --all      # both Linux and macOS targets (must run on macOS)

import { $ } from "bun"
import { copyFileSync, mkdirSync, statSync } from "node:fs"
import { join } from "node:path"

const args = new Set(process.argv.slice(2))
for (const a of args) {
  if (a !== "--linux" && a !== "--darwin" && a !== "--all") {
    console.error(`build-host: unknown option ${a} (use --linux, --darwin, or --all)`)
    process.exit(2)
  }
}

const wantLinux = args.has("--linux")
const wantDarwin = args.has("--darwin")
const wantAll = args.has("--all")
const buildLinux = wantLinux || wantAll || (!wantDarwin && process.platform !== "darwin")
const buildDarwin = wantDarwin || wantAll || (!wantLinux && process.platform === "darwin")

const root = (await $`git rev-parse --show-toplevel`.text()).trim()
process.chdir(root)

const linuxTargets: [string, string][] = [
  ["x86_64-unknown-linux-musl", "x86_64-linux"],
  ["aarch64-unknown-linux-musl", "aarch64-linux"],
]

const darwinTargets: [string, string][] = [
  ["aarch64-apple-darwin", "aarch64-darwin"],
  ["x86_64-apple-darwin", "x86_64-darwin"],
]

const out = join(root, "app/src-tauri/host")
mkdirSync(out, { recursive: true })

function copyTargets(targets: [string, string][]) {
  for (const [target, platform] of targets) {
    const from = join(root, "target", target, "release", "moonfish-host")
    const to = join(out, `moonfish-host-${platform}`)
    copyFileSync(from, to)
    console.log(`${to} (${Math.round(statSync(to).size / 1024)} KiB)`)
  }
}

if (buildLinux) {
  if ((await $`cargo zigbuild --help`.quiet().nothrow()).exitCode !== 0) {
    console.error("build-host: cargo-zigbuild isn't installed (pip install cargo-zigbuild)")
    process.exit(1)
  }
  await $`rustup target add ${linuxTargets.map(([t]) => t)}`.nothrow()
  await $`cargo zigbuild --release -p moonfish-host ${linuxTargets.flatMap(([t]) => ["--target", t])}`
  copyTargets(linuxTargets)
}

if (buildDarwin) {
  if (process.platform !== "darwin") {
    console.error("build-host: --darwin targets must be built on macOS")
    process.exit(1)
  }
  await $`rustup target add ${darwinTargets.map(([t]) => t)}`.nothrow()
  await $`cargo build --release -p moonfish-host ${darwinTargets.flatMap(([t]) => ["--target", t])}`
  copyTargets(darwinTargets)
}

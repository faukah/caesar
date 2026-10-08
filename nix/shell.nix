# SPDX-License-Identifier: EUPL-1.2

{
  mkShell,
  cargo,
  rustc,
  clippy,
  rust-analyzer-unwrapped,
  rustfmt,
  rustPlatform,
  taplo,
}:
mkShell {
  name = "caesar";
  strictDeps = true;
  nativeBuildInputs = [
    cargo
    rustc
    clippy
    rust-analyzer-unwrapped
    (rustfmt.override { asNightly = true; })
    taplo
  ];
  env.RUST_SRC_PATH = rustPlatform.rustLibSrc;
}

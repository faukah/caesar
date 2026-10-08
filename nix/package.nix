# SPDX-License-Identifier: EUPL-1.2

{
  lib,
  rustPlatform,
}:
let
  cargoToml = lib.importTOML ../Cargo.toml;
in
rustPlatform.buildRustPackage {
  pname = "caesar";
  inherit (cargoToml.package) version;

  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.lock
      ../Cargo.toml
      ../src
    ];
  };

  cargoLock.lockFile = ../Cargo.lock;

  meta = {
    description = "Small CalDAV and CardDAV server for personal use";
    homepage = "https://github.com/faukah/caesar";
    license = lib.licenses.eupl12;
    mainProgram = "caesar";
    platforms = lib.platforms.linux;
  };
}

# SPDX-License-Identifier: EUPL-1.2

{
  inputs.nixpkgs.url = "github:NixOS/nixpkgs?ref=nixos-unstable";

  outputs =
    {
      self,
      nixpkgs,
      ...
    }:
    let
      forEachSystem = nixpkgs.lib.genAttrs [
        "x86_64-linux"
        "aarch64-linux"
      ];
      pkgsForEach = nixpkgs.legacyPackages;
    in
    {
      nixosModules = {
        caesar = import ./nix/module.nix self;
        default = self.nixosModules.caesar;
      };

      packages = forEachSystem (
        system:
        let
          caesar = pkgsForEach.${system}.callPackage ./nix/package.nix { };
        in
        {
          inherit caesar;
          default = caesar;
        }
      );

      devShells = forEachSystem (system: {
        default = pkgsForEach.${system}.callPackage ./nix/shell.nix { };
      });

      formatter = forEachSystem (system: pkgsForEach.${system}.nixfmt-tree);
    };
}

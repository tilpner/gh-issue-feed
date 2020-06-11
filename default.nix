{ }:

let
  nixpkgs = fetchTarball {
    # 2020-06-11 nixos-unstable, pinned because 20.03 is incompatible with crate2nix master
    url = "https://github.com/NixOS/nixpkgs/archive/029a5de08390bb03c3f44230b064fd1850c6658a.tar.gz";
    sha256 = "03fjkzhrs2avcvdabgm7a65rnyjaqbqdnv4q86qyjkkwg64g5m8x";
  };

  pkgs = import nixpkgs {
    config = {};
    overlays = [];
  };

  workspace = pkgs.callPackage ./Cargo.nix {};
in workspace.workspaceMembers.github-label-feed.build

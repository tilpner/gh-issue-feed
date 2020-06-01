{ pkgs ? import <nixpkgs> {} }:

(pkgs.callPackage ./Cargo.nix {}).workspaceMembers.github-label-feed.build

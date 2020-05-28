with import <nixpkgs> {};

let
  llvmPackages = llvmPackages_9;
in mkShell {
  RUSTFLAGS="-C link-arg=-fuse-ld=lld";

  buildInputs = [
    pkgconfig
    openssl

    nur.repos.mozilla.rustChannels.stable.rust
    crate2nix

    cacert

    sqlite
  ] ++ (with llvmPackages; [
    clang llvm
    libclang.lib
    lldClang
  ]);
}

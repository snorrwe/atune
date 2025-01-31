_default:
    @just --list

check:
    nix flake check -v -L --all-systems


# build the atune docker image from the nix flake
build-docker:
    nix build .#dockerImage

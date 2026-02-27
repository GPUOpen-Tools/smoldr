# Copyright (c) Advanced Micro Devices, Inc., or its affiliates.
#
# SPDX-License-Identifier: MIT
#
# Modified from https://github.com/nix-community/naersk/blob/master/examples/cross-windows/flake.nix
#
# The MIT License (MIT)
#
# Copyright 2019 Nicolas Mattia
#
# Permission is hereby granted, free of charge, to any person obtaining a copy
# of this software and associated documentation files(the "Software"), to deal
# in the Software without restriction, including without limitation the rights
# to use, copy, modify, merge, publish, distribute, sublicense, and / or sell
# copies of the Software, and to permit persons to whom the Software is
# furnished to do so, subject to the following conditions :
#
# The above copyright notice and this permission notice shall be included in all
# copies or substantial portions of the Software.
#
# THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
# IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
# FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT.IN NO EVENT SHALL THE
# AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
# LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
# OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
# SOFTWARE.

# Run tests: nix flake check
# Run the windows/mingw version: nix run .#mingw examples/AddTwo.sm
# Build a release: nix build .#mingw-release
# Get a shell with all dependencies: nix develop

{
  inputs = {
    fenix.url = "github:nix-community/fenix";
    flake-utils.url = "github:numtide/flake-utils";
    naersk.url = "github:nix-community/naersk";
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
  };

  outputs = { self, fenix, flake-utils, naersk, nixpkgs }:
    flake-utils.lib.eachSystem [ flake-utils.lib.system.x86_64-linux ] (system:
      let
        pkgs = (import nixpkgs) {
          inherit system;
        };
        lib = pkgs.lib;

        dxcVersion = "1.8.2505";
        dxcDate = "2025_05_24";
        dxc = pkgs.fetchurl {
          url = "https://github.com/microsoft/DirectXShaderCompiler/releases/download/v${dxcVersion}/dxc_${dxcDate}.zip";
          hash = "sha256-gTgPPsoVbZAtZAT9bfn0sIhvV2/z4YsswQ0wdf/J0Rk=";
        };

        agilitySdkVersion = "1.717.1-preview";
        agilityPkgName = "Microsoft.Direct3D.D3D12";
        agilitySdk = pkgs.fetchurl {
          url = "https://www.nuget.org/api/v2/package/${agilityPkgName}/${agilitySdkVersion}";
          hash = "sha256-8FLflDfH5HzbQo/7WXSB/lWkgMiWiwBWFZ/joZXCd+Q=";
        };

        vkd3d-protonVersion = "2.10";
        vkd3d-proton = pkgs.fetchurl {
          url = "https://github.com/HansKristian-Work/vkd3d-proton/releases/download/v${vkd3d-protonVersion}/vkd3d-proton-${vkd3d-protonVersion}.tar.zst";
          hash = "sha256-8dzVdOFqre7uK3QENKr6PWiRpnxBhp6nubenHPs+7bU=";
        };

        native-toolchain = with fenix.packages.${system}; combine [
          minimal.rustc
          minimal.cargo
          complete.clippy
          latest.rustfmt
        ];

        mingw-toolchain = with fenix.packages.${system}; combine [
          minimal.rustc
          minimal.cargo
          targets.x86_64-pc-windows-gnu.latest.rust-std
        ];

        naersk-lib = naersk.lib.${system}.override {
          cargo = native-toolchain;
          rustc = native-toolchain;
        };
        naersk-lib-win = naersk.lib.${system}.override {
          cargo = mingw-toolchain;
          rustc = mingw-toolchain;
        };

        defaultBuildArgs = {
          src = self;
          strictDeps = true;

          # Deny warnings and clippy warnings
          RUSTFLAGS = [ "-Dwarnings" ];
        };

        winBuildArgs = lib.recursiveUpdate defaultBuildArgs {
          # Build with mingw
          depsBuildBuild = with pkgs; [
            pkgsCross.mingwW64.stdenv.cc
            pkgsCross.mingwW64.windows.pthreads
          ];

          nativeCheckInputs = with pkgs; [
            # We need Wine to run tests:
            wineWowPackages.stable
          ];

          # Maybe with proton and sway virtual screen
          # Run with VKD3D_DEBUG=trace VKD3DCONFIG=vk_debug,dxr DISPLAY=:2
          doCheck = false;

          # Tells Cargo that we're building for Windows.
          # (https://doc.rust-lang.org/cargo/reference/config.html#buildtarget)
          CARGO_BUILD_TARGET = "x86_64-pc-windows-gnu";

          # Fix: some `extern` functions couldn't be found; some native libraries may need to be installed or have their path specified
          # when using C dependencies
          TARGET_CC = "x86_64-w64-mingw32-gcc";
          TARGET_CXX = "x86_64-w64-mingw32-g++";

          # Tells Cargo that it should use Wine to run tests.
          # (https://doc.rust-lang.org/cargo/reference/config.html#targettriplerunner)
          CARGO_TARGET_X86_64_PC_WINDOWS_GNU_RUNNER = pkgs.writeShellScript "wine-wrapper" ''
            export WINEPREFIX="$(mktemp -d)"
            exec wine64 "$@"
          '';

          overrideMain = oldAttrs: oldAttrs // {
            # Add dxc libraries
            postInstall = ''
              mkdir dxc
              pushd dxc
              ${pkgs.unzip}/bin/unzip "${dxc}"
              mv bin/x64/*.dll $out/bin/
              popd
            '';
          };
        };

        smoldr = naersk-lib.buildPackage (lib.recursiveUpdate defaultBuildArgs {
          nativeCheckInputs = with pkgs; [
            # The null backend calls dxc
            directx-shader-compiler
            cargo-deny
          ];

          doCheck = true;

          # Do not build dx12 backend
          cargoBuildOptions = opts: opts ++ [ "--no-default-features" ];
          cargoTestOptions = opts: opts ++ [ "--no-default-features" ];

          cargoTestCommands = tests: tests ++ [
            # Run clippy lints
            "cargo $cargo_options clippy $cargo_test_options --all-targets"
            # Check formatting
            "cargo $cargo_options fmt --all --check"
            # Check licenses
            "cargo deny --offline check bans licenses sources"
          ];
        });

        package-win = naersk-lib-win.buildPackage winBuildArgs;

        package-win-agility = naersk-lib-win.buildPackage (lib.recursiveUpdate winBuildArgs {
          overrideMain = oldAttrs: oldAttrs // {
            D3D12SDK_VERSION = "717";
            preConfigure = ''
              cargo_build_options="$cargo_build_options --features agility_sdk"
            '';

            # Add dxc libraries
            postInstall = ''
              mkdir dxc
              pushd dxc
              ${pkgs.unzip}/bin/unzip "${dxc}"
              mv bin/x64/*.dll $out/bin/
              popd

              mkdir D3D12
              pushd D3D12
              ${pkgs.unzip}/bin/unzip "${agilitySdk}"
              mkdir -p $out/bin/D3D12
              mv build/native/bin/x64/*.dll $out/bin/D3D12/
              popd
            '';
          };
        });

        # TODO Extract vkd3d-proton in temporary directory
        app-win-drv = pkgs.writeShellScriptBin "smoldr" ''
          set -euo pipefail

          # Create wine prefix
          export WINEPREFIX="$(mktemp -d)"
          cleanup() {
            # Shutdown wine
            ${pkgs.wineWowPackages.stable}/bin/wineboot -s
            # Wait until finished
            ${pkgs.wineWowPackages.stable}/bin/wineserver -w
            # Remove prefix
            rm -rf "$WINEPREFIX"
          }
          trap cleanup EXIT

          # Disable "install wine-mono" dialogue
          export WINEDLLOVERRIDES="mscoree="
          ${pkgs.wineWowPackages.stable}/bin/wineboot -i
          # Wait until ready
          ${pkgs.wineWowPackages.stable}/bin/wineserver -w

          # Install dx12
          PATH="${pkgs.zstd}/bin:$PATH" ${pkgs.gnutar}/bin/tar xf ${vkd3d-proton}
          PATH="${pkgs.wineWowPackages.stable}/bin:$PATH" ${pkgs.runtimeShell} vkd3d-proton-${vkd3d-protonVersion}/setup_vkd3d_proton.sh install
          echo Wine prefix is ready
          echo

          ${pkgs.wineWowPackages.stable}/bin/wine64 ${package-win}/bin/smoldr.exe "$@"
          # Wait until finished
          ${pkgs.wineWowPackages.stable}/bin/wineserver -w
        '';
        app-win = flake-utils.lib.mkApp {
          name = "smoldr";
          drv = app-win-drv;
        };

      in rec {
        packages.default = packages.smoldr;
        packages.smoldr = smoldr;
        packages.mingw = package-win;
        packages.mingw-agility = package-win-agility;

        packages.mingw-release = pkgs.runCommand "create-mingw-release" {} ''
          mkdir -p $out
          mkdir -p smoldr
          cp -a ${package-win}/bin/* smoldr/
          ${pkgs.zip}/bin/zip -r $out/smoldr.zip smoldr
        '';

        packages.mingw-agility-release = pkgs.runCommand "create-mingw-release" {} ''
          mkdir -p $out
          mkdir -p smoldr
          cp -a ${package-win-agility}/bin/* smoldr/
          ${pkgs.zip}/bin/zip -r $out/smoldr.zip smoldr
        '';

        apps.default = apps.smoldr;
        apps.smoldr = flake-utils.lib.mkApp {
          name = "smoldr";
          drv = packages.smoldr;
        };
        apps.mingw = app-win;

        checks.build-native = packages.smoldr;
        checks.build-mingw = app-win-drv;
        checks.mingw-release = packages.mingw-release;
        checks.mingw-agility-release = packages.mingw-agility-release;
        checks.mingw-help = pkgs.runCommand "check-help" {} ''
          ${app-win-drv}/bin/smoldr --help
          mkdir -p $out
        '';

        checks.typos = pkgs.runCommand "check-typos" {} ''
          ${pkgs.typos}/bin/typos ${self}
          mkdir -p $out
        '';

        checks.parse-examples = pkgs.stdenv.mkDerivation {
          name = "parse-examples";

          src = self;

          nativeBuildInputs = with pkgs; [
            directx-shader-compiler
            python3Minimal
            packages.smoldr
          ];

          buildPhase = ''
            python run_scripts.py -a=--ignore-expect smoldr examples
            mkdir -p $out
          '';
        };
      }
    );
}

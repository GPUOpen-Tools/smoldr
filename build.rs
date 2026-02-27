// Copyright (c) Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT OR Apache-2.0

#[cfg(feature = "agility_sdk")]
use std::env;

fn main() {
    #[cfg(feature = "enable_cpp")]
    {
        // Compile C++ helpers
        cxx_build::bridge("src/backend/dx12.rs")
            .file("src/backend/Dx12Helpers.cpp")
            .flag_if_supported("-std=c++17")
            .flag_if_supported("/std:c++17")
            .compile("dx12helpers");
        println!("cargo:rerun-if-changed=src/backend/dx12.rs");
        println!("cargo:rerun-if-changed=src/backend/Dx12Helpers.cpp");
        println!("cargo:rerun-if-changed=src/backend/Dx12Helpers.h");
    }

    #[cfg(feature = "agility_sdk")]
    {
        // Explicitly specify exports to support the Agility SDK
        if env::var("CARGO_CFG_WINDOWS").is_ok() {
            if env::var("CARGO_CFG_TARGET_ENV").as_ref().map(String::as_ref) == Ok("gnu") {
                // mingw
                println!("cargo:rustc-link-arg=windows.def");
            } else {
                // msvc
                println!("cargo:rustc-link-arg=/DEF:windows.def");
            }
        }
    }
}

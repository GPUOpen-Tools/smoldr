// Copyright (c) Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT OR Apache-2.0

#pragma once

#include <exception>

#include <d3d12.h>
#include <dxc/dxcapi.h>
#include <winrt/base.h>

#include "rust/cxx.h"

namespace rust {
namespace behavior {

template <typename Try, typename Fail>
static void trycatch(Try &&func, Fail &&fail) noexcept {
    try {
        func();
    } catch (const std::exception &e) {
        fail(e.what());
    } catch (const winrt::hresult_error &e) {
        fail(winrt::to_string(e.message()).c_str());
    }
}

} // namespace behavior
} // namespace rust

IDxcOperationResult *compile(IDxcCompiler3 *compiler, rust::Str code, rust::Slice<const uint16_t * const> args);

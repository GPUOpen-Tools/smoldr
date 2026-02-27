// Copyright (c) Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT OR Apache-2.0

#include "Dx12Helpers.h"

#include "rust/cxx.h"

IDxcOperationResult *compile(IDxcCompiler3 *compiler_ptr, rust::Str code, rust::Slice<const uint16_t * const> args) {
    winrt::com_ptr<IDxcCompiler3> compiler;
    compiler.attach(compiler_ptr);

    DxcBuffer Source;
    Source.Ptr = code.data();
    Source.Size = code.size();
    Source.Encoding = DXC_CP_UTF8;

    winrt::com_ptr<IDxcUtils> utils;
    winrt::com_ptr<IDxcIncludeHandler> includeHandler;
    DxcCreateInstance(CLSID_DxcUtils, IID_PPV_ARGS(&utils));
    utils->CreateDefaultIncludeHandler(includeHandler.put());

    winrt::com_ptr<IDxcResult> results;
    winrt::check_hresult(compiler->Compile(
        &Source,
        const_cast<LPCWSTR *>(reinterpret_cast<const LPCWSTR *>(args.data())),
        args.size(),
        includeHandler.get(),
        IID_PPV_ARGS(&results)));

    return results.detach();
}

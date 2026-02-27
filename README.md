# Smoldr

A simple scripting tool to run DX12 shaders on HW using a text input file. Inspired by the [Amber](https://github.com/google/amber) tool for Vulkan.

## What is Smoldr?

Smoldr allows compiling HLSL source through DXIL, creating pipelines, resources, views, and then binding and running a compute dispatch.
This is all controlled by a text based script file - no C++ development needed.

The project is work-in-progress; the script format is not fixed and may change.
Basic functionality like compute shaders are working and raytracing pipelines are supported.
New features are added as needed, but there is no roadmap or plans to support more features.
If there is a feature that is missing for you, pull-requests are welcome.

## How does it work?

The [documentation](./Documentation.md) shows how to write scripts.

Examples can be found in the [`examples`](./examples) directory.

### Example

Here’s an example of how a script looks like.
The script takes two input buffers, adds them together and stores the result into an output buffer.

```python
# Create a HLSL source called csshader
SOURCE csshader
ByteAddressBuffer inbuf[2] : register(t0); // SRV
RWByteAddressBuffer outbuf : register(u0); // UAV

[numthreads(32, 1, 1)]
void CSMain(uint3 DTid : SV_DispatchThreadID)
{
  // Take 2 numbers from first buffer, one from second, and sum them
  unsigned int first_idx = DTid.x * 2;
  float first = inbuf[0].Load<float>(first_idx * 4) + inbuf[0].Load<float>((first_idx + 1) * 4);
  float sum = first + inbuf[1].Load<float>(DTid.x * 4);
  outbuf.Store<float>(DTid.x * 4, sum);
}
END

# Compile the source with dxc into a binary called csobj
OBJECT csobj csshader cs_6_4 CSMain

# Allocate buffers in GPU memory for input and output
BUFFER inbuf DATA_TYPE float SIZE 64 SERIES_FROM 0 INC_BY .25
BUFFER inbuf2 DATA_TYPE float SIZE 32 SERIES_FROM 10.0 INC_BY .25
BUFFER outbuf DATA_TYPE float SIZE 32 FILL 0

# The root signature
ROOT default
  TABLE UAV REGISTER 0 NUMBER 1 SPACE 0
  TABLE SRV REGISTER 0 NUMBER 2 SPACE 0
END

# Create a compute pipeline called cspipe
PIPELINE cspipe COMPUTE
  ATTACH csobj
  ROOT   default
END

# Create views that point to the complete buffers
VIEW inview inbuf AS SRV
VIEW inview2 inbuf2 AS SRV
VIEW outview outbuf AS UAV

# Run the pipeline in a 1x1x1 dispatch
DISPATCH cspipe
  BIND 0 TABLE outview
  BIND 1 TABLE inview
RUN 1 1 1

# Check that the shaders worked as expected
EXPECT outbuf float OFFSET 0 EQ 10.25 11.5 12.75 14
EXPECT outbuf float OFFSET 64 EQ 30.25 31.5 32.75 34

# Use DUMP to see a buffer's content
#DUMP outbuf float
```

## Use

To execute a script, run
```bash
./smoldr examples/AddTwo.sm
```

To enable the DirectX debug layer for better error messages, use
```bash
./smoldr --validate examples/AddTwo.sm
```

For capturing, it can be useful to display a window and run the script in a frame with a present call.
`--window` by runs the script once per frame, without limit by default.
The number of frames can be specified with `--repeat <num>`.
```bash
./smoldr --window --repeat 5 examples/AddTwo.sm
```

## Requirements

Build dependencies:
- [Rust](https://rust-lang.org), preferred installation method is [rustup](https://rustup.rs)

Run `cargo build` for a debug build and `cargo build --release` for a release build.
The binary is created in `target/<debug|release>`.

Runtime dependencies:
- `dxil.dll` and `dxcompiler.dll` from the DirectX Shader Compiler need to be put in the same folder as `smoldr.exe`. The latest release can be downloaded [here](https://github.com/microsoft/DirectXShaderCompiler/releases).

### Agility SDK

Microsoft sometimes publishes an [Agility SDK](https://devblogs.microsoft.com/directx/gettingstarted-dx12agility), which is a DirectX runtime that can be shipped with an application.
This SDK can be useful to test preview features.
smoldr can be built with support for a specific SDK version.
The SDK version needs to be specified during the build, as it needs to be encoded in the application.
This automatically enables support for experimental shader models and therefore needs the running machine to be in developer mode.

Build smoldr on Windows with
```powershell
$env:D3D12SDK_VERSION = "<version number>"
cargo build --features agility_sdk
```

The Agility SDK libraries need to be put into a `D3D12/` folder near `smoldr.exe`.
`D3D12Core.dll` is required in the `D3D12/` folder.
For debug layer support, `d3d12SDKLayers.dll` is required in addition.

## Test

Run unit tests with `cargo test`.

To check if all example scripts are parsed correctly (pass `-a=--ignore-expect` when running on Linux):
```bash
./run_scripts.py target/debug/smoldr examples
```

## Develop

The DirectX 12 backend uses the Rust bindings of the Windows API. Documentation for that can be found here: https://microsoft.github.io/windows-docs-rs/doc/windows/Win32/Graphics/Direct3D12/

### Code Structure

The code in `src/parser.rs` parses a file and transforms it into a list of `Directive`s, which are declared in `src/main.rs`.
These statements are then run by a backend, which is either `dx12` or `null`, implemented in the `src/backend` directory.
The `null` backend does not actually run pipelines, but it enables running and testing the tool on Linux.

## Linux

Smoldr can also be compiled and run on Linux with the `null` backend.

To run natively on Linux, pass `--no-default-features` to cargo (disables dx12 support).
Via Wine and vkd3d-proton, the Windows executable can be run on Linux and Vulkan.

The `null` backend, which is the only backend supported natively on Linux, needs `dxc` (the [DirectX Shader Compiler](https://github.com/microsoft/DirectXShaderCompiler)) in the `PATH`.
As dispatches are ignored and not actually run on a GPU, the `--ignore-expect` flag can be passed to also ignore `EXPECT` lines and make tests pass with the `null` backend.
Scripts can be run in such a way with `cargo run --no-default-features -- --ignore-expect examples/<script.sm>`.

## License

Licensed under either of

 * [Apache License, Version 2.0](LICENSE-APACHE)
 * [MIT license](LICENSE-MIT)

at your option.

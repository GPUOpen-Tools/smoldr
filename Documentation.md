Language Spec
==============

## Introduction

This tool is intended as a (relatively) easy way to author DX12 compiler focused tests, inspired by a similar [Amber](https://github.com/google/amber) tool for Vulkan. This document intends to describe the language for implementing these tests.

- [Language Spec](#language-spec)
  - [Introduction](#introduction)
  - [Basic Usage](#basic-usage)
  - [Examples](#examples)
  - [Reference](#reference)
    - [Identifiers](#identifiers)
    - [Strings](#strings)
    - [Known data types](#known-data-types)
    - [Producing DXIL Shader Representation](#producing-dxil-shader-representation)
      - [SOURCE](#source)
      - [OBJECT](#object)
      - [LIB](#lib)
    - [Binding Layouts](#binding-layouts)
      - [ROOT](#root)
        - [TABLE](#table)
        - [UAV/SRV](#uavsrv)
        - [ROOT\_CONST](#root_const)
      - [Flags](#flags)
        - [Dynamic Resource](#dynamic-resource)
      - [Example](#example)
      - [Extract From DXIL](#extract-from-dxil)
        - [ROOT\_DXIL](#root_dxil)
    - [Command Signatures](#command-signatures)
        - [UAV/SRV](#uavsrv-1)
        - [ROOT\_CONST](#root_const-1)
        - [Dispatch Type](#dispatch-type)
    - [Building a Pipeline or State Object](#building-a-pipeline-or-state-object)
      - [Traditional Pipelines](#traditional-pipelines)
        - [PIPELINE](#pipeline)
      - [Pipeline State Object](#pipeline-state-object)
        - [LIB](#lib-1)
        - [COLLECTION](#collection)
        - [CONFIG](#config)
        - [HIT\_GROUP](#hit_group)
        - [EXPORTS](#exports)
        - [Example](#example-1)
    - [Dispatching a Pipeline or State Object](#dispatching-a-pipeline-or-state-object)
      - [DISPATCH](#dispatch)
      - [DISPATCHRAYS](#dispatchrays)
      - [EXECUTE_INDIRECT](#execute_indirect)
      - [Dispatch Properties](#dispatch-properties)
      - [BIND](#bind)
      - [ROOT\_CONST](#root_const-2)
      - [UAV/SRV](#uavsrv-1)
      - [ROOT_SIG](#root_sig)
      - [Example](#example-2)
    - [Memory allocation on the GPU](#memory-allocation-on-the-gpu)
      - [BUFFER](#buffer)
        - [Example](#example-3)
      - [VIEW](#view)
        - [Untyped Buffer View](#untyped-buffer-view)
        - [Typed Buffer View](#typed-buffer-view)
        - [Structured Buffer View](#structured-buffer-view)
        - [Raytracing Acceleration Structure](#raytracing-acceleration-structure)
        - [Example](#example-4)
    - [Values](#values)
      - [DATA\_TYPE - Array Initialization](#data_type---array-initialization)
      - [RAW - Freeform initialization](#raw---freeform-initialization)
      - [RAW data specification](#raw-data-specification)
    - [Ray Tracing (DXR)](#ray-tracing-dxr)
      - [BLAS](#blas)
      - [TLAS](#tlas)
      - [Shader Binding Tables](#shader-binding-tables)
        - [SHADERTABLE](#shadertable)
        - [RECORD](#record)
        - [SHADERID](#shaderid)
        - [TABLE](#table-1)
        - [GPUVA](#gpuva)
        - [Example](#example-5)
      - [SHADERID](#shaderid-1)
    - [Miscellaneous](#miscellaneous)
      - [Include Files](#include-files)
      - [SLEEP](#sleep)
    - [Print and Check Buffers](#print-and-check-buffers)
      - [DUMP](#dump)
      - [EXPECT](#expect)
      - [ASSERT](#assert)


## Basic Usage

Running code on a GPU using the DirectX 12 API involves a lot of boiler plate and low level configuration.
The goal of this tool is to hide most of that.
With this simplified interface the steps to run code on the GPU are roughly this:

* Compile source HLSL to DXIL
* Define a binding layout ("root signature" in DX12 speak)
* Create a pipeline from the DXIL and binding information
* Allocate (and potentially initialize) memory on the GPU
* Potentially define descriptors referencing this memory on the GPU
* Bind and execute the pipeline
* Read the results back from GPU memory

>_Note: This tool is missing a lot of useful features, most notably support for Graphics pipelines. It currently supports Compute Shader pipelines, and Ray Tracing pipelines. The intention is this tool will continue to grow to support more use cases over time._

The Reference below is ordered in roughly the order described above.

## Reference

A script file is a list of statements, where a statement usually executes something (i.e. compiles a shader) and/or leads to a result that is given a name and can be used in later statements.

### Identifiers

Identifiers are used to assign names to results of statements and reference them in later statements.
An identifier consists of one or more characters.
It must start with a letter or an underscore and can continue with letters, underscores, or digits.

### Strings

Strings that are used as arguments in statements, e.g. for shader names.
They can be quoted or unquoted.
In unquoted form, a string is taken literally until the next whitespace character.
In quoted form, within double quotes, there are the following escape sequences:

- `\\` → `\`
- `\"` → `"`
- `\xx`, where `xx` is a hex number, is replaced by the character represented by the hex number

Examples

```
abc
123
"abc"
"\""
"\\"
"\01abc"
```

### Known data types

Where data types are specified in the scripting language, the following are currently accepted types.

* `uint64`
* `uint32`
* `uint16`
* `uint8`
* `float`
* `float16`

### Producing DXIL Shader Representation

Commands to specify source, and compile to various DXIL targets.

#### SOURCE

```
SOURCE <source_identifier>
  <text>
END
```

Define the source code of an HLSL shader.

* `source_identifier` is a string to identify the source in later script commands.
* `text` between the `SOURCE` command and `END` line is source code.
* `END` on it's own line indicates the end of the source section.

Example

```
SOURCE cs_source
RWByteAddressBuffer outbuf : register(u0);

[numthreads(32, 1, 1)]
void CSMain()
{
  outbuf.Store<float>(0, 2.0);
}
END
```

#### OBJECT

```
OBJECT <object_identifier> <source_identifier> <target_profile> <entry> [<dxc_options>]
```

Call DXC to compile HLSL source to a DXIL object. (Not a DXIL library, use `LIB` for libraries.)
* `object_identifier` is a string used to identify the produced object in later commands.
* `source_identifier` refers to a source object previously defined in the script. This is the source that will be compiled.
* `target_profile` is the HLSL/DX12 target profile as provided to DXC (e.g., `cs_6_4`).
* `entry` is the name of the entry function for the compiled shader.
* `dxc_options` optional space separated set of options to pass to DXC (can be used for defines, etc.).

Example

```
OBJECT cs_obj cs_source cs_6_4 CSMain
```

#### LIB

```
LIB <object_identifier> <source_identifier> <target_profile> [<dxc_options>]
```

Call DXC to compile HLSL source to a DXIL library.
* `object_identifier` is a string used to identify the produced object in later commands.
* `source_identifier` refers to a source object previously defined in the script. This is the source that will be compiled.
* `target_profile` is the HLSL/DX12 target profile as provided to DXC (e.g., `lib_6_4`).
* `dxc_options` optional space separated set of options to pass to DXC (can be used for defines, etc.).

Example

```
LIB lib source lib_6_4
```

### Binding Layouts

It is beyond the scope of this document to fully describe DX12's [binding model](https://learn.microsoft.com/en-us/windows/win32/direct3d12/root-signatures). 

In the briefest terms, connections to application data the shader may want to read or write are modeled as global variables in the HLSL source code. The **root signature** defines the order of arguments to the shader, and how they map onto those global variables. The order is defined by information provided by the application at DXIL to ISA compile time. When the application executes the shader, it must provide the promised information in the order it specified.

The inputs can be in several forms:

* `Root constants` which are constant values mapped onto a constant buffer structure in the source.
* `Root descriptors` which are effectively pointers to memory (although they are not modeled as pointers in HLSL). As they really are pointers underneath, only simple buffers without bounds checking can be provided this way.
* `Descriptors` which are provided as pointers to a (table of) descriptors in memory. There is a large table in DX12 called the **Descriptor Heap** into which the application must create descriptors. _(See `VIEW` command.)_ These descriptors might represent simple buffers (with bounds checking), typed buffers (auto type conversion on load/store), or textures of various sorts. The input to the shader is a pointer to an entry in this **Descriptor Heap** table. If the binding specifies that it is a table of descriptors, this pointer refers to the first entry of the table.

#### ROOT
```
ROOT <root_identifier>
  <signature>
END
```
Define a root signature layout.

* `root_identifier` A string used to identify the produced signature in later commands.
* `signature` A sequence of lines (documented below) defining the root signature. The order of these lines defines the order of the arguments in the signature, implicitly bound to argument numbers.
* `END` ends the root signature definition.

##### TABLE

```
TABLE <type> REGISTER <register> NUMBER <num> SPACE <space>
```

Defines a root signature entry that refers to `Descriptors` in the **descriptor heap**.

* `type` is one of
  * `UAV` "Unordered Access View" aka read-write buffer
  * `SRV` "Structured Resource View" aka read-only buffer
* `register` is the register number in HLSL source.
* `num` is the number of descriptors in the table (1 for a single descriptor).
* `space` is the descriptor space (see DX12 docs for details. Use `0` in most cases).

##### UAV/SRV

```
<root_type> REGISTER <register> SPACE <space>
```

Define a root signature entry that will be a pointer to GPU memory.

* `root_type` is one of
  * `UAV` "Unordered Access View" aka read-write buffer
  * `SRV` "Structured Resource View" aka read-only buffer
* `register` is the register number in HLSL source.
* `space` is the descriptor space (see DX12 docs for details. Use `0` in most cases).

##### ROOT_CONST

```
ROOT_CONST NUMBER <dwords> REGISTER <register> SPACE <space>
```

Define a root signature entry that will be constant values

* `dwords` number of dwords of constant values that will be provided.
* `register` is the register number in HLSL source.
* `space` is the descriptor space (see DX12 docs for details. Use `0` in most cases).

#### Flags

##### Dynamic Resource

```
CONFIG [allow_input_assembler_input_layout] [deny_vertex_shader_root_access] [deny_hull_shader_root_access] [deny_domain_shader_root_access] [deny_geometry_shader_root_access] [deny_pixel_shader_root_access] [allow_stream_output] [local_root_signature] [deny_amplification_shader_root_access] [deny_mesh_shader_root_access] [cbv_srv_uav_heap_directly_indexed] [sampler_heap_directly_indexed]
```

Set the `D3D12_ROOT_SIGNATURE_FLAG_CBV_SRV_UAV_HEAP_DIRECTLY_INDEXED` flag (or others) on the root signature, to allow using the Dynamic Resource feature for resources within the associated shader/pipeline.

#### Example

HLSL source with
* `inbuf` Table of 2 read only input buffers (SRV)
* `outbuf` A read-write buffer (UAV)
* `mycb` A constant buffer structure


```hlsl
ByteAddressBuffer inbuf[2] : register(t0);  // SRV
RWByteAddressBuffer outbuf : register(u5);  // UAV
cbuffer mycb               : register(b0) { // Constant Buffer
  float a; unsigned int b;
};
```

Root signature defining an order of inputs to the shader as
* Pointer to a `UAV` buffer #5 (register(u5))
* Reference to a table of `SRV` descriptors for SRV #0 (register(t0))
* Two DWords of constants that will map onto constant buffer #0 (register(b0))
* Enable Dynamic Resource indexing for the Resource Heap

```
ROOT default
  UAV REGISTER 5 SPACE 0
  TABLE SRV BASE 0 NUMBER 2 SPACE 0
  ROOT_CONST NUMBER 2 REGISTER 0 SPACE 0
  CONFIG cbv_srv_uav_heap_directly_indexed
END
```

#### Extract From DXIL

HLSL can have a root signature specified in source as well. The example root signature defined by the `ROOT default` example above can instead be defined as below.

```hlsl
GlobalRootSignature root =
{
  "UAV(u5),"                                      // UAV
  "DescriptorTable(SRV(t0, numDescriptors = 2))," // SRV table
  "RootConstants(num32BitConstants=2, b0)"        // Two 32-bit constants
};
```

##### ROOT_DXIL

```
ROOT_DXIL <root_identifier> <object_identifier>
```

Extract a root signature from a DXIL library object.

* `root_identifier` a string that can be used in later commands to reference the created root signature
* `object_identifier` the name of the previously created object from which to extract the root signature

Example

```
ROOT_DXIL root_sig obj
```


### Command Signatures

```
COMMAND_SIGNATURE <signature_identifier> [STRIDE <stride>] [ROOT_SIG <root_identifier>]
  <signature>
  <dispatch_type>
END
```

For `ExecuteIndirect`, a command signature that defines the structure of the argument buffer needs to be defined.

* `signature_identifier` is the identifier assigned to the created command signature.
* `stride` is the stride in bytes in the argument buffer when multiple dispatches of this signature are dispatched in a single `ExecuteIndirect`.
  If not specified, the minimum stride is computed from the signature.
* `root_identifier` is an optional root signature and must be specified if the `signature` changes values from the root signature.
* `signature` specifies which fields of the root signature are changed as documented below.
* `dispatch_type` is the type of dispatch as documented below.

Example

```
COMMAND_SIGNATURE sig STRIDE 4 ROOT_SIG root
  SRV REGISTER 1
  UAV REGISTER 2
  ROOT_CONST NUMBER 3 REGISTER 3 OFFSET 8
  DISPATCH
END

```

#### UAV/SRV

```
<view_type> REGISTER <register>
```

Specify that the argument buffer will have a 64-bit GPU address of a view in this place.

* `view_type` is either `SRV` or `UAV` depending on the view type.
* `register` is the register from the root signature that is set.

#### ROOT_CONST

```
ROOT_CONST NUMBER <dwords> REGISTER <register> OFFSET <offset>
```

* `dwords` is the number of 32-bit constant values that will be provided.
* `register` is the register from the root signature that is set.
* `offset` is the offset in the root signatures from which the constants will be set.

#### Dispatch Type

`DISPATCH` specifies that a compute dispatch is launched.
The argument buffer contains three 32-bit integers in this place for `x`, `y` and `z` dispatch dimensions (the `D3D12_DISPATCH_ARGUMENTS` struct).

`DISPATCHRAYS` specifies that a raytracing dispatch is launched.
The argument buffer contains the `D3D12_DISPATCH_RAYS_DESC` struct in this place.


### Building a Pipeline or State Object

DXIL objects can be compiled into pipelines in combination with a root signature. In recent APIs, RT allows creating Collections or Pipeline State Objects from DXIL libraries to support more complex ways of organizing the code.

#### Traditional Pipelines

Traditional pipelines are compute and graphics pipelines that do not use the pipeline state objects.

##### PIPELINE

```
PIPELINE <pipeline_identifier> <type>
  ATTACH <object_identifier>
  ROOT   <root_identifier>
END
```

Create a pre-PSO pipeline.

* `pipeline_identifier` a string that can be used in later commands to reference the created pipeline
* `type` the type of pipeline. Currently only `COMPUTE` is supported
* `object_identifier` name of a previously created DXIL object containing the compute shader to implement the pipeline
* `root_identifier` name of a previously defined root signature to specify the binding for the pipeline
* `END` on it's own line indicates the end of the pipeline specification

Example

```
PIPELINE pipeline COMPUTE
  ATTACH cs_obj
  ROOT root_sig
END
```


#### Pipeline State Object

Pipeline State Objects are a new api to create pipelines.
This is used for raytracing.

```
COLLECTION <pso_identifier> [ADDTO <addto>]
  <pso_properties>
END
```

Create a collection state object.

```
RTPSO <pso_identifier> [ADDTO <addto>]
  <pso_properties>
END
```

Create a raytracing pipeline state object.

* `pso_identifier` is the identifier assigned to the created object.
* `addto` can be specified optionally to add onto an existing state object (this corresponds to the `AddToStateObject` API).
* `pso_properties` specify properties of the state object as documented below.

##### LIB

```
LIB <object_identifier> [EXPORTS <exports>]
```

Add a library to the pipeline state object.

* `object_identifier` is a compiled object created with `LIB`.
* `exports` specify function and other exports and renames in the format documented below.

##### COLLECTION

```
COLLECTION <pso_identifier> [EXPORTS <exports>]
```

Add a collection to the pipeline state object.

* `pso_identifier` is a state object created with `COLLECTION`.
* `exports` specify function and other exports and renames in the format documented below.

##### CONFIG

```
CONFIG [local_dep_on_external] [external_dep_on_local] [add_to_so]
```

Set the `D3D12_STATE_OBJECT_FLAG_ALLOW_LOCAL_DEPENDENCIES_ON_EXTERNAL_DEFINITIONS` flag (or others) on the pipeline state object.

##### HIT_GROUP

```
HIT_GROUP <hitgroup_identifier> <anyhit_shader> <closesthit_shader> <intersection_shader>
```

Specify a hit group for the pipeline state object.

* `hitgroup_identifier` is the name used for the hit group.
* `anyhit_shader` is the name of the any hit shader in the hit group. It can be omitted by specifying `-`.
* `closesthit_shader` is the name of the closest hit shader in the hit group. It can be omitted by specifying `-`.
* `intersection_shader` is the name of the intersection shader in the hit group. It can be omitted by specifying `-`.
  If an intersection shader is specified, the hit group is a procedural hit group.
  If `-` is used (no intersection shader), it is a triangle hit group.

##### EXPORTS

```
EXPORTS [<name>|<name>=<to_rename>]*
```

Specify exports and renames for the library or collection.

* `name` is the name under which the object (shader or hit group) is visible in the new pipeline state object.
* `to_rename` is the name under which the object is visible in the library or collection it is imported from.
  `to_rename` is optional and defaults to `name`.

##### Example

```
COLLECTION col
  LIB rtobj
END

RTPSO rtpso
  LIB rtobj2 EXPORTS foo=foo_orig bar
  COLLECTION col
END
```


### Dispatching a Pipeline or State Object

After building a pipeline, it can be used in dispatches.

#### DISPATCH

```
DISPATCH <pipeline_identifier>
  <dispatch_properties>
RUN <dim_x> <dim_y> <dim_z>
```

Dispatch a pipeline to the GPU.

* `pipeline_identifier` specifies the used pipeline.
* `dispatch_properties` is a sequence of lines (documented below) that allow changing properties of the dispatch.
* `dim_x` is the `x` dimension of the dispatch.
* `dim_y` is the `y` dimension of the dispatch.
* `dim_z` is the `z` dimension of the dispatch.

#### DISPATCHRAYS

```
DISPATCHRAYS <pipeline_identifier>
  <dispatch_properties>
RUN <table_raygeneration> <table_hit> <table_miss> <table_callable> <dim_x> <dim_y> <dim_z>
```

Dispatch a raytracing pipeline to the GPU.
Takes [Shader Binding Tables](#shader-binding-tables) for different shader types as arguments.

* `pipeline_identifier` specifies the used pipeline.
* `dispatch_properties` is a sequence of lines (documented below) that allow changing properties of the dispatch.
* `table_raygeneration` is the shadertable used for the ray generation shader.
* `table_hit` is the `shadertable_identifier` used for hit groups.
* `table_miss` is the `shadertable_identifier` used for miss shaders.
* `table_callable` is the `shadertable_identifier` used for callable shaders.
* `dim_x` is the `x` dimension of the dispatch.
* `dim_y` is the `y` dimension of the dispatch.
* `dim_z` is the `z` dimension of the dispatch.

#### EXECUTE_INDIRECT

```
EXECUTE_INDIRECT <pipeline_identifier> SIGNATURE <signature_identifier>
  <dispatch_properties>
RUN <argument_buffer> [OFFSET <argument_offset>] MAX_COMMANDS <max_commands> [COUNT <count_buffer> [COUNT_OFFSET <count_offset>]]
```

Runs an indirect dispatch where the argument buffer contains dispatch data at execution time.

* `pipeline_identifier` specifies the used pipeline. Depending on the dispatch type, either a traditional pipeline or a pipeline state object.
* `signature_identifier` specifies the command signature of the argument buffer.
* `dispatch_properties` is a sequence of lines (documented below) that allow changing properties of the dispatch.
* `argument_buffer` is the buffer that contains the command content.
* `argument_offset` is an optional offset into the `argument_buffer` to where the command starts.
* `max_commands` is the number of commands that are listed in the `argument_buffer`.
* `count_buffer` optionally contains the number of commands to execute. The minimum of the number from the buffer and the specified `max_commands` is executed.
* `count_offset` optionally specifies an offset into the `count_buffer`.

#### Dispatch Properties

#### BIND

```
BIND <index> TABLE <view_identifier>
```

Binds a view to the dispatch.

* `index` is the index in the root signature where this is bound.
* `view_identifier` is the view that is bound.

#### ROOT_CONST

```
ROOT_CONST <index> <initialization>
```

Set root constants.

* `index` is the index in the root signature where this is bound.
* `initialization` describes how the memory should be initialized, see the [Values](#values) documentation.

#### UAV/SRV

```
[UAV|SRV] <index> <buffer_identifier>
```

Binds a buffer read-write (UAV) or read-only (SRV).

* `index` is the index in the root signature where this is bound.
* `buffer_identifier` is the buffer that is bound.

#### ROOT_SIG

```
ROOT_SIG <root_identifier>
```

Specify the root signature used for the dispatch.
For traditional pipelines, it defaults to the root signature specified in the pipeline.

* `root_identifier` is the root signature.

#### Example

```
DISPATCH pipeline
  BIND 0 TABLE view
  ROOT_SIG root_sig
  ROOT_CONST 1 RAW 4
    uint32 5
  END
RUN 1 1 1

DISPATCHRAYS rtpso
  BIND 0 TABLE view
  ROOT_SIG root_sig
RUN rgen_table - miss_table - 64 1 1
```


### Memory allocation on the GPU

In order to provide input and output space for shaders, memory must be allocated on the GPU. This is currently limited to Buffer memory.

#### BUFFER

```
BUFFER <resource_identifier> <initialization>
```

Allocate a buffer on the GPU, potentially initializing it.

* `resource_identifier` is the identifier that can be used in later commands to reference the created resource.
* `initialization` describes how the memory should be initialized, see the [Values](#values) documentation.

##### Example

```
BUFFER buf DATA_TYPE uint32 SIZE 5 FILL 0
BUFFER buf DATA_TYPE uint32 SIZE 5 SERIES_FROM 0 INC_BY 1
BUFFER buf RAW 128
  uint32 3 4
  float 1.0
  GPUVA buffer
END
```

#### VIEW

```
VIEW <view_identifier> <buffer_identifier> AS <view_description>
```

Declare a view to a buffer.

* `view_identifier` is the name assigned to the view.
* `buffer_identifier` is the memory buffer the view points to.
* `view_description` specifies the type and other properties of the view as documented below.
  `UAV` in the description is a read-write view, `SRV` is a read-only view.

##### Untyped Buffer View

```
[UAV|SRV]
```

Declares an untyped buffer view.

##### Typed Buffer View

```
TYPED [UAV|SRV] <type>
```

Declares a typed buffer view.

* `type` is the data type of the view.

##### Structured Buffer View

```
STRUCTURED [UAV|SRV] BYTES <byte_size>
```

Declares a structured buffer view.

* `byte_size` is the size of the struct in bytes.

##### Raytracing Acceleration Structure

```
RTAS SRV
```

Declares an acceleration structure view for raytracing.
The buffer must have been created with the `TLAS` statement.

##### Example

```
VIEW view buf AS UAV
VIEW view buf AS TYPED UAV float
VIEW view buf AS STRUCTURED SRV BYTES 16
VIEW rtas tlas AS RTAS SRV
```

### Values

Values can be specified as typed or untyped.
Both representations are converted to a list of bytes, the only difference between the notations is the syntax.

#### DATA_TYPE - Array Initialization

```
DATA_TYPE <type> SIZE <element_size> <initialization_values>
```

Initialize the buffer, filling it as if it was an array of values of a specified type.

* `type` type of the array to initialize. Note, this does not necessarily need to match how the buffer will be used by the shader. This simply defines how the allocated memory will be filled.
* `element_size` declares the number of elements of `<type>` that are allocated for the buffer.
* `initialization_values` defines how the array will be filled.

```
DATA_TYPE <type> SIZE <size> FILL <value>
```

Fill the array such that every element has the same value

* `value` value to fill the array with.

```
DATA_TYPE <type> SIZE <size> SERIES_FROM <start> INC_BY <increment>
```

Fill the array with increasing values.

* `start` value of the first element in the array.
* `increment` each element in the array will be the previous element incremented by this value.

#### RAW - Freeform initialization

```
RAW <byte_size>
  <raw_values>
END
```

Initialize with control over offsets and types of specific initialization values.

* `byte_size` is number of bytes to allocate for the buffer.
* `raw_values` specification of values and offsets _(See [RAW](#raw-data-specification) specification)_.
* `END` indicates end of raw value buffer initialization.

#### RAW data specification

In some contexts, the scripting language allows a "raw" specification of the contents of a block of memory.

```
<type> <values>
```

Write a sequence of values into the buffer, starting at the current offset.

* `type` the type of the values to write into the buffer.
* `values` a space separated series of values to write into the buffer. As each value is written, the offset is incremented by the size of `type`.

A special type is `GPUVA <buffer>` which writes the 64-bit GPU address of the specified buffer.


Example

```
BUFFER struct RAW 128
  uint32 3 4
  float 1.0
  GPUVA buffer
END
```

Allocates a buffer of size `128` bytes, and writes the `uint32` values `3` and `4` into offsets `0` and `4` respectively, the `float` value `1.0` at offset `8` and the 64-bit GPU address of `buffer` at offset 12.

### Ray Tracing (DXR)

Ray tracing introduces the need for additional concepts. These include

* Acceleration Structure - the description of the scene geometry
* Shader Binding Tables - binding of "shader" and data to objects hit in the scene

The tool currently provides the ability to create RT scenes in order to write simple unit tests.

#### BLAS

It is possible to specify geometry by listing the instances and their triangles.
`BLAS` defines a bottom-level acceleration structure with multiple geometries.

```
BLAS <blas_identifier>
  GEOMETRY TRIANGLE
    VERTEX <x> <y> <z>
    CONFIG [no_duplicate_anyhit] [opaque]
  END

  GEOMETRY PROCEDURAL
    AABB <min_x> <min_y> <min_z> <max_x> <max_y> <max_z>
    CONFIG [no_duplicate_anyhit] [opaque]
  END

  CONFIG [prefer_fast_build] [prefer_fast_trace] [allow_compaction] [allow_update] [minimize_memory]
END
```

* `blas_identifier` is the identifier used for the bottom-level acceleration structure.
* `x`, `y`, and `z` are the three-dimensional coordinates for a vertex. The number of vertices in a `TRIANGLE` geometry need to be a multiple of three.
* `min_*` and `max_*` `x`, `y`, and `z` specify the bounds of an axis-aligned bounding box for procedural geometry.
* Flags like `D3D12_RAYTRACING_GEOMETRY_FLAG_NO_DUPLICATE_ANYHIT_INVOCATION` or others can be set for per geometry.
* Flags like `D3D12_RAYTRACING_ACCELERATION_STRUCTURE_BUILD_FLAG_PREFER_FAST_TRACE` or others can be set for the acceleration structure.

Example

```
BLAS triangle_blas
  GEOMETRY TRIANGLE
    VERTEX 0.0 -0.75 1.0
    VERTEX -0.75 0.75 1.0
    VERTEX 0.75 0.75 1.0
  END

  # Geometry with two triangles
  GEOMETRY TRIANGLE
    VERTEX 0.0 -0.75 1.0
    VERTEX -0.75 0.75 1.0
    VERTEX 0.75 0.75 1.0

    VERTEX 1.0 -0.75 1.0
    VERTEX -1.75 0.75 1.0
    VERTEX 1.75 0.75 1.0
  END
END

BLAS procedural_blas
  GEOMETRY PROCEDURAL
    AABB -1 -1 -1 1 1 1
    # No any hit calls
    CONFIG opaque
  END
```

#### TLAS

The top-level acceleration structure is defined by `TLAS`.

```
TLAS <tlas_identifier>
  BLAS <blas_identifier> -

  BLAS <blas_identifier>
    ID <id>
    MASK <mask>
    HIT_GROUP_INDEX_CONTRIBUTION <i>
    TRANSFORM
      <transform>
    END
    CONFIG [triangle_cull_disable] [triangle_front_counterclockwise] [force_opaque] [force_non_opaque]
  END

  CONFIG [prefer_fast_build] [prefer_fast_trace] [allow_compaction] [allow_update] [minimize_memory]
END
```

* `tlas_identifier` is the identifier used for the top-level acceleration structure.
* `blas_identifier` references a bottom-level acceleration structure for the instance.
* `id` is the instance id of the bottom-level acceleration structure. It defaults to `0`.
* `mask` is the instance mask for the bottom-level acceleration structure. It defaults to `0xff`.
* `i` is the contribution of the instance to the hit group index. It defaults to `0`.
* `transform` is a 3x4 matrix that transforms the instance acceleration structure. It defaults to the identity matrix.
* Flags like `D3D12_RAYTRACING_INSTANCE_FLAG_TRIANGLE_CULL_DISABLE` or others can be set per instance.
* Flags like `D3D12_RAYTRACING_ACCELERATION_STRUCTURE_BUILD_FLAG_PREFER_FAST_TRACE` or others can be set for the acceleration structure.

Example

```
TLAS as
  BLAS triangle_blas -

  # Add a second instance
  BLAS triangle_blas
    # Set the InstanceContributionToHitGroupIndex
    HIT_GROUP_INDEX_CONTRIBUTION 1
    TRANSFORM
      1 0 0 0
      0 1 0 0
      0 0 1 0
    END
  END
END
```


#### Shader Binding Tables

Shader binding tables are buffers that contain shader identifiers and local root signatures.

##### SHADERTABLE

Raytracing collects shaders in the form of shader identifiers in tables, called shader binding tables.
A shader binding table is used during a dispatch, to call the shader that is referenced by the shader identifier at a computed index.

```
SHADERTABLE <shadertable_identifier> <pso_identifier>
  <records>
END
```

Create a shader binding table containing the specified shader identifiers and local root signatures.

* `shadertable_identifier` is the name assigned to the shader binding table.
* `pso_identifier` specifies the pipeline state object where the shaders are part of, if they are referenced by name.
* `records` are the content of the shader binding table (documented below).

##### RECORD

```
RECORD <index> [<name>]
  <record_properties>
END
```

```
RECORD <index> <name> -
```

Create a record in a shader binding table.

* `index` is the index in the shader table that the record is written to.
  The byte offset is determined by multiplying the index with the maximum shader record size in a table.
* `name` is the shader or hit group name that is used to get the shader identifier for this record from the pipeline state object associated with the table.
  The `name` is optional, a shader id can also be omitted (writing a `null` shader id) or specified in the properties as `SHADERID`.
* `record_properties` allow to specify the shader id by identifier and the content of the local root signature.

###### SHADERID

```
SHADERID <shaderid_identifier>
```

Specify the shader identifier for the shader table record.
This is optional and can be used as an alternative to specifying the shader or hit group name directly.

* `shaderid_identifier` references the shader id to use.

###### TABLE

```
TABLE <view_identifier>
```

Add a view to the local root signature.

* `view_identifier` is the added view.

###### GPUVA

```
GPUVA <buffer_identifier>
```

Add a buffer to the local root signature.

* `buffer_identifier` is the added buffer.

##### Example

```
SHADERTABLE miss_table rtpso
  RECORD 0 miss0 -

  RECORD 1 miss1
    TABLE view
    GPUVA buf
  END

  RECORD 1
    SHADERID miss_id
  END

  # Null record
  RECORD 2
  END
END
```

#### SHADERID

```
SHADERID <shaderid_identifier> <pso_identifier> <name>
```

Get the shader identifier by the name of a shader or hit group.

* `shaderid_identifier` is the name assigned to the fetched shader identifier.
* `pso_identifier` specifies the pipeline state object where the shader is part of.
* `name` is the name of the shader (for a ray generation, miss, or callable shader) or the name of the hit group.

Example

```
SHADERID miss_id rtpso miss
```


### Miscellaneous

Statements that are useful for writing scripts.

#### Include Files

To reduce repetition of common code between scripts, a script can include another script.
All objects defined before the `INCLUDE` are available to the included script and
all objects defined inside the `INCLUDED` script are available after the script is included, similar to how `#include "path"` works in C.

```
INCLUDE <path>
```

* `path` the path to the file that should be included, relative to the current script

Example

```
INCLUDE triangle_tlas.sm
```


#### SLEEP

```
SLEEP <duration>
```

Sleep and do nothing for the specified duration.
This can be useful when debugging driver issues that may be timing related.

* `duration` of the sleep is specified as multiple numbers and short units.

Example

```
SLEEP 1s
SLEEP 2s 500ms
SLEEP 1us
```


### Print and Check Buffers

The content of buffers can be printed or checked.
This can be used for example to test that a dispatch wrote expected values into a buffer.

#### DUMP

```
DUMP <buffer_identifier> <type> [PRINT_STRIDE <stride>] [EXPECT]
```

Dump a buffer to standard output.

* `buffer_identifier` specifies the buffer that is printed.
* `type` defines the data type that should be used for printing.
* `PRINT_STRIDE` can be added to print the output on multiple lines. Each line then has `<stride>` elements in it.
* `EXPECT` can be added to print an `EXPECT` line for the current buffer content (see format below)

Example

```
DUMP buf uint32
DUMP buf float EXPECT
DUMP buf float PRINT_STRIDE 8
DUMP buf uint32 PRINT_STRIDE 4 EXPECT
```

#### EXPECT

```
EXPECT <buffer_identifier> <type> [EPSILON <epsilon>] OFFSET <byte_offset> EQ <values>
```

Assert that a buffer contains the expected data.

* `buffer_identifier` specifies the buffer that is printed.
* `type` defines the data type that should be used for printing.
* `epsilon` is an optionally allowed difference between the expected values and the actual buffer content.
* `byte_offset` is the position in the buffer, where `values` should begin.
* `values` is a space-separated list of numbers of `type` that are checked for equality with the current buffer content.

Example

```
EXPECT buf uint32 OFFSET 0 EQ 1 2 3 4
EXPECT buf float EPSILON 0.1 OFFSET 4 EQ 1.0 2.0
```

#### ASSERT

```
ASSERT SHADERID [EQ|NE] <shaderid_a> <shaderid_b>
```

Assert that two shader identifiers are equal or different.

* `shaderid_a` references the first shader identifier, as obtained by `SHADERID`.
* `shaderid_b` references the second shader identifier, as obtained by `SHADERID`.

```
ASSERT SHADERID EQ miss0_id miss1_id
ASSERT SHADERID NE miss_id rgen_id
```

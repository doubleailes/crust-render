# hdCrust — the Crust Render Hydra render delegate plugin.
#
# Needs: an OpenUSD C++ build (imaging enabled; GL not required) and the
# crust-capi cdylib from this repository (`cargo build --release -p
# crust-capi`). Typical configure:
#
#   cmake -S hydra/hdCrust -B build/hdCrust \
#         -DCMAKE_PREFIX_PATH=$USD_ROOT \
#         -DCRUST_CAPI_DIR=$PWD/target/release \
#         -DCMAKE_INSTALL_PREFIX=$PWD/install
#   cmake --build build/hdCrust --target install
#
# See README.md for the usdview launch environment.

cmake_minimum_required(VERSION 3.20)
project(hdCrust LANGUAGES CXX)

find_package(pxr REQUIRED)

set(CRUST_CAPI_DIR "" CACHE PATH
    "Directory containing the crust_capi library (cargo's target/release)")
# On Windows, cargo's cdylib produces crust_capi.dll + crust_capi.dll.lib
# (the import library, found by the "crust_capi.dll" name) alongside the
# staticlib crust_capi.lib — prefer the import library so the plugin links
# against the DLL instead of pulling the whole engine in statically (which
# would also need Rust's system libraries spelled out by hand). On
# Linux/macOS the first name matches nothing and libcrust_capi.so/.dylib
# resolves via the second.
find_library(CRUST_CAPI_LIB
    NAMES crust_capi.dll crust_capi
    HINTS "${CRUST_CAPI_DIR}"
    REQUIRED)
# include/crust.h lives in the crate, two levels up from target/release by
# default; allow an explicit override.
set(CRUST_INCLUDE_DIR "${CMAKE_CURRENT_SOURCE_DIR}/../../crates/crust-capi/include"
    CACHE PATH "Directory containing crust.h")

add_library(hdCrust SHARED
    instancer.cpp
    light.cpp
    material.cpp
    mesh.cpp
    renderBuffer.cpp
    renderDelegate.cpp
    renderPass.cpp
    rendererPlugin.cpp
)

set_target_properties(hdCrust PROPERTIES
    PREFIX "lib"
    CXX_STANDARD 17
    CXX_STANDARD_REQUIRED ON
)

target_include_directories(hdCrust PRIVATE "${CRUST_INCLUDE_DIR}")
# `hd` transitively pulls the pxr base/usd libraries it needs.
target_link_libraries(hdCrust PRIVATE hd "${CRUST_CAPI_LIB}")

# plugInfo.json carries the platform's actual library file name
# (libhdCrust.so / libhdCrust.dylib / libhdCrust.dll).
set(HDCRUST_LIBRARY_FILE "libhdCrust${CMAKE_SHARED_LIBRARY_SUFFIX}")
configure_file(plugInfo.json.in "${CMAKE_CURRENT_BINARY_DIR}/plugInfo.json" @ONLY)

# Install layout: point PXR_PLUGINPATH_NAME at
# <prefix>/plugin/usd/hdCrust/resources (see README.md).
# RUNTIME covers Windows, where a SHARED library's .dll installs as a
# runtime artifact; LIBRARY covers .so/.dylib.
install(TARGETS hdCrust
    LIBRARY DESTINATION plugin/usd/hdCrust
    RUNTIME DESTINATION plugin/usd/hdCrust)
install(FILES "${CMAKE_CURRENT_BINARY_DIR}/plugInfo.json"
    DESTINATION plugin/usd/hdCrust/resources)

# Headless smoke test (no GL, no usdview): loads the plugin through the
# registry, renders a cube via hd's unit-test scene delegate, asserts the
# color buffer converged nonzero. Run with PXR_PLUGINPATH_NAME pointing at
# the installed resources dir.
option(HDCRUST_BUILD_TESTS "Build the headless hdCrust smoke test" OFF)
if(HDCRUST_BUILD_TESTS)
    add_executable(testHdCrust tests/testHdCrust.cpp)
    set_target_properties(testHdCrust PROPERTIES
        CXX_STANDARD 17
        CXX_STANDARD_REQUIRED ON)
    target_include_directories(testHdCrust PRIVATE "${CRUST_INCLUDE_DIR}")
    target_link_libraries(testHdCrust PRIVATE hd cameraUtil "${CRUST_CAPI_LIB}")
endif()

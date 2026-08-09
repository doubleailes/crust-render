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
    "Directory containing libcrust_capi.so (cargo's target/release)")
find_library(CRUST_CAPI_LIB
    NAMES crust_capi
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

# Install layout: point PXR_PLUGINPATH_NAME at
# <prefix>/plugin/usd/hdCrust/resources (see README.md).
install(TARGETS hdCrust
    LIBRARY DESTINATION plugin/usd/hdCrust)
install(FILES plugInfo.json
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

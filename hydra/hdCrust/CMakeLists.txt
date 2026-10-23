# hdCrust: crust as a Hydra render delegate (the hydra-delegate capability,
# openspec/specs/hydra-delegate). Built outside `cargo build`, against the
# OpenUSD the host runs (hydra/pixi.toml): `find_package(pxr)`, then crust
# itself as the `crust-hydra` static library, built here with cargo under
# the `hydra` profile (panic = "unwind": a crust panic is an error, not an
# abort of the host).
#
#   cd hydra && pixi run cmake -S hdCrust -B build -G Ninja -DCMAKE_BUILD_TYPE=Release
#   pixi run cmake --build build && pixi run cmake --install build --prefix install
#   PXR_PLUGINPATH_NAME=<prefix>/plugin/usd/hdCrust/resources usdview scene.usda
#
# On Windows, configure from an MSVC (vcvars64) shell.
cmake_minimum_required(VERSION 3.26)
project(hdCrust LANGUAGES CXX)

# Installed binaries keep the paths they linked OpenUSD's libraries from
# (hdCrustTest runs from the install tree; CMake strips RPATH otherwise).
set(CMAKE_INSTALL_RPATH_USE_LINK_PATH ON)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

find_package(pxr REQUIRED)

# ---- crust-hydra, built by cargo ----------------------------------------
# The workspace root is two levels up; cargo decides whether anything needs
# rebuilding, so the command runs on every build (it is quick when nothing
# changed).
get_filename_component(CRUST_ROOT "${CMAKE_CURRENT_SOURCE_DIR}/../.." ABSOLUTE)
set(CRUST_TARGET_DIR "${CMAKE_CURRENT_BINARY_DIR}/cargo")
if(WIN32)
    set(CRUST_HYDRA_LIB "${CRUST_TARGET_DIR}/hydra/crust_hydra.lib")
    # What Rust's std needs from the system when linked into a C++ binary
    # (`cargo rustc -p crust-hydra --profile hydra -- --print native-static-libs`).
    set(CRUST_HYDRA_SYSTEM_LIBS kernel32 advapi32 ntdll userenv ws2_32 bcrypt dbghelp)
else()
    set(CRUST_HYDRA_LIB "${CRUST_TARGET_DIR}/hydra/libcrust_hydra.a")
    set(CRUST_HYDRA_SYSTEM_LIBS pthread dl m)
endif()
find_program(CARGO cargo REQUIRED)
add_custom_target(crust_hydra_cargo
    COMMAND "${CARGO}" build --profile hydra -p crust-hydra
            --manifest-path "${CRUST_ROOT}/Cargo.toml"
            --target-dir "${CRUST_TARGET_DIR}"
    WORKING_DIRECTORY "${CRUST_ROOT}"
    BYPRODUCTS "${CRUST_HYDRA_LIB}"
    COMMENT "cargo build --profile hydra -p crust-hydra"
    USES_TERMINAL)
add_library(crust_hydra STATIC IMPORTED)
set_target_properties(crust_hydra PROPERTIES
    IMPORTED_LOCATION "${CRUST_HYDRA_LIB}"
    INTERFACE_INCLUDE_DIRECTORIES "${CRUST_ROOT}/crates/crust-hydra/include")
add_dependencies(crust_hydra crust_hydra_cargo)

# ---- the plugin ----------------------------------------------------------
add_library(hdCrust SHARED
    crustAttributesAdapter.cpp
    renderBuffer.cpp
    renderDelegate.cpp
    rendererPlugin.cpp
    renderPass.cpp
    sceneWriter.cpp
    session.cpp
)
# Tf's registry functions are keyed by the library's name.
target_compile_definitions(hdCrust PRIVATE
    MFB_PACKAGE_NAME=hdCrust
    MFB_ALT_PACKAGE_NAME=hdCrust
    MFB_PACKAGE_MODULE=HdCrust)
if(MSVC)
    target_compile_options(hdCrust PRIVATE /Zc:inline- /bigobj /W3)
    target_compile_definitions(hdCrust PRIVATE NOMINMAX)
endif()
target_link_libraries(hdCrust PRIVATE
    hd hdx usdImaging usdGeom usdLux usdRender usdShade sdf usd tf gf vt plug
    crust_hydra ${CRUST_HYDRA_SYSTEM_LIBS})
set_target_properties(hdCrust PROPERTIES PREFIX "")

# hdCrustTest (D9): a stage rendered through the delegate headless, its
# color buffer written as an EXR for `crust diff` (test/compare.sh).
add_executable(hdCrustTest test/hdCrustTest.cpp)
target_compile_definitions(hdCrustTest PRIVATE MFB_PACKAGE_NAME=hdCrustTest
                           MFB_ALT_PACKAGE_NAME=hdCrustTest)
if(MSVC)
    target_compile_options(hdCrustTest PRIVATE /Zc:inline- /bigobj)
    target_compile_definitions(hdCrustTest PRIVATE NOMINMAX)
endif()
target_link_libraries(hdCrustTest PRIVATE
    usdImaging hdsi hd cameraUtil usdGeom usd sdf tf gf vt)

# The layout OpenUSD's own plugins use: the library in plugin/usd, its
# plugInfo.json in plugin/usd/hdCrust/resources, pointing back at it.
# Relative to Root (plugin/usd/hdCrust): the library beside that directory.
set(PLUG_INFO_LIBRARY_PATH "../hdCrust${CMAKE_SHARED_LIBRARY_SUFFIX}")
set(PLUG_INFO_RESOURCE_PATH "resources")
set(PLUG_INFO_ROOT "..")
configure_file(plugInfo.json "${CMAKE_CURRENT_BINARY_DIR}/plugInfo.json" @ONLY)

install(TARGETS hdCrustTest RUNTIME DESTINATION bin)
install(TARGETS hdCrust
    RUNTIME DESTINATION plugin/usd
    LIBRARY DESTINATION plugin/usd)
install(FILES "${CMAKE_CURRENT_BINARY_DIR}/plugInfo.json"
    DESTINATION plugin/usd/hdCrust/resources)

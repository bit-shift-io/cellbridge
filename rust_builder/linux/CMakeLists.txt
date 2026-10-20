# The Flutter tooling requires that developers have CMake 3.10 or later
# installed. You should not increase this version, as doing so will cause
# the plugin to fail to compile for some customers of the plugin.
cmake_minimum_required(VERSION 3.10)
cmake_policy(SET CMP0079 NEW)

# Project-level configuration.
# Keep the CMake project name in sync with the plugin package name: Flutter's
# generated `generated_plugins.cmake` reads
# `${cellbridge_builder_bundled_libraries}` to copy the Rust cdylib into the
# bundle. The crate name stays `cellbridge` (the third argument below).
set(PROJECT_NAME "cellbridge_builder")
project(${PROJECT_NAME} LANGUAGES CXX)

include("../cargokit/cmake/cargokit.cmake")
apply_cargokit(${PROJECT_NAME} ../../rust cellbridge "")
target_link_libraries(${BINARY_NAME} PRIVATE "${${PROJECT_NAME}_cargokit_lib}")

# List of absolute paths to libraries that should be bundled with the plugin.
# This list could contain prebuilt libraries, or libraries created by an
# external build triggered from this build file.
set(${PROJECT_NAME}_bundled_libraries
  "${${PROJECT_NAME}_cargokit_lib}"
  PARENT_SCOPE
)

# Licensed under the Apache-2.0 license
# SPDX-License-Identifier: Apache-2.0

load("@rules_rust//rust:defs.bzl", _rust_binary = "rust_binary", _rust_library = "rust_library")

# Deprecated transition wrapper rules.
# Transport flags are now configured globally in .bazelrc, allowing standard rust_binary and rust_library to be used.

def opentitan_rust_binary(name, **kwargs):
    _rust_binary(name = name, **kwargs)

def opentitan_rust_library(name, **kwargs):
    _rust_library(name = name, **kwargs)

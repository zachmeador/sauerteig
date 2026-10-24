#!/bin/sh
# Source this from the repository root, or set SAUERTEIG_ROOT first.
SAUERTEIG_ROOT=${SAUERTEIG_ROOT:-"$(pwd)"}
if [ -x "$SAUERTEIG_ROOT/.tools/cargo/bin/cargo" ]; then
  export RUSTUP_HOME="$SAUERTEIG_ROOT/.tools/rustup"
  export CARGO_HOME="$SAUERTEIG_ROOT/.tools/cargo"
  case ":$PATH:" in
    *":$CARGO_HOME/bin:"*) ;;
    *) export PATH="$CARGO_HOME/bin:$PATH" ;;
  esac
fi

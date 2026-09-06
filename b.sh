cd /mnt/c/Users/jeremy.DOMAIN/Desktop/ferrite
export CARGO_TARGET_DIR=/tmp/ferrite-target
export RUSTUP_HOME=/home/jeremy/.rustup
export CARGO_HOME=/home/jeremy/.cargo
export PATH="/home/jeremy/.cargo/bin:$PATH"
cargo fmt -p ferrite-pool
cargo clippy -p ferrite-pool --all-targets -- -D warnings 2>&1 | grep -E "^(error|warning)" -A 10 | head -60

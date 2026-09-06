cd /mnt/c/Users/jeremy.DOMAIN/Desktop/ferrite
export CARGO_TARGET_DIR=/tmp/ferrite-target
export RUSTUP_HOME=/home/jeremy/.rustup
export CARGO_HOME=/home/jeremy/.cargo
export PATH="/home/jeremy/.cargo/bin:$PATH"
BIN=$(cargo test -p ferrite-pool --test mount --no-run --message-format=json 2>&1 | grep -o '"executable":"[^"]*mount[^"]*"' | tail -1 | cut -d'"' -f4)
"$BIN" --ignored --nocapture --test-threads=1 an_acl_set_through_the_pool 2>&1 | head -20

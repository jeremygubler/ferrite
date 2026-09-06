cd /mnt/c/Users/jeremy.DOMAIN/Desktop/ferrite
export CARGO_TARGET_DIR=/tmp/ferrite-target
export RUSTUP_HOME=/home/jeremy/.rustup
export CARGO_HOME=/home/jeremy/.cargo
export PATH="/home/jeremy/.cargo/bin:$PATH"
run() {
  BIN=$(cargo test -p ferrite-pool --test mount --no-run --message-format=json 2>&1 | grep -o '"executable":"[^"]*mount[^"]*"' | tail -1 | cut -d'"' -f4)
  if [ -z "$BIN" ]; then echo "BAUT NICHT"; return; fi
  "$BIN" --ignored --test-threads=1 2>&1 | grep -E "^test .* FAILED|^test result"
}
echo "=== 3b. Wert am Nullbyte abgeschnitten (in backing::set_xattr) ==="
cp pool/src/fuse/backing.rs /tmp/backing.bak
perl -0pi -e 's{    let path = c_path\(&branch\.resolve\(relative\)\)\?;\n    check\(\n        unsafe \{\n            libc::lsetxattr\(}{    let path = c_path(&branch.resolve(relative))?;\n    let value = &value[..value.iter().position(|b| *b == 0).unwrap_or(value.len())];\n    check(\n        unsafe \{\n            libc::lsetxattr(}' pool/src/fuse/backing.rs
run
cp /tmp/backing.bak pool/src/fuse/backing.rs
echo "=== wiederhergestellt ==="; run

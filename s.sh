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
echo "=== 0. unversehrt ==="; run

echo "=== 1. jedes Handle bekommt eine eigene backing_id (der alte Fehler) ==="
cp pool/src/fuse/server.rs /tmp/server.bak
perl -0pi -e 's{        if let Some\(handed\) = self\.backing\.get_mut\(&key\) \{}{        if let Some(handed) = None::<&mut Handed> \{}' pool/src/fuse/server.rs
run
cp /tmp/server.bak pool/src/fuse/server.rs

echo "=== 2. beim ersten release schon schliessen ==="
cp pool/src/fuse/server.rs /tmp/server.bak
perl -0pi -e 's{        handed\.holders -= 1;\n        if handed\.holders > 0 \{\n            return;\n        \}}{        handed.holders -= 1;}' pool/src/fuse/server.rs
run
cp /tmp/server.bak pool/src/fuse/server.rs

echo "=== 3. nie schliessen ==="
cp pool/src/fuse/server.rs /tmp/server.bak
perl -0pi -e 's{        if crate::fuse::connection::backing_close\(connection, id\) \{}{        if false \&\& crate::fuse::connection::backing_close(connection, id) \{}' pool/src/fuse/server.rs
run
cp /tmp/server.bak pool/src/fuse/server.rs

echo "=== wiederhergestellt ==="; run

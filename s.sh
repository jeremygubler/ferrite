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

echo "=== 1. FUSE_DONT_MASK nicht angemeldet ==="
cp pool/src/fuse/server.rs /tmp/server.bak
perl -0pi -e 's{flags \|= request\.flags & \(abi::FUSE_POSIX_ACL \| abi::FUSE_DONT_MASK\);}{flags |= request.flags \& abi::FUSE_POSIX_ACL;}; s{flags & abi::FUSE_POSIX_ACL != 0 && flags & abi::FUSE_DONT_MASK != 0;}{flags \& abi::FUSE_POSIX_ACL != 0;}' pool/src/fuse/server.rs
run
cp /tmp/server.bak pool/src/fuse/server.rs

echo "=== 2. creation_mode zieht nie ab ==="
cp pool/src/fuse/server.rs /tmp/server.bak
perl -0pi -e 's{        if backing::has_default_acl\(root, &parent\) \{\n            mode\n        \} else \{\n            mode & !umask\n        \}}{        let _ = (root, parent, umask);\n        mode}' pool/src/fuse/server.rs
run
cp /tmp/server.bak pool/src/fuse/server.rs

echo "=== 3. creation_mode zieht immer ab ==="
cp pool/src/fuse/server.rs /tmp/server.bak
perl -0pi -e 's{        if backing::has_default_acl\(root, &parent\) \{\n            mode\n        \} else \{\n            mode & !umask\n        \}}{        let _ = (root, parent);\n        mode & !umask}' pool/src/fuse/server.rs
run
cp /tmp/server.bak pool/src/fuse/server.rs

echo "=== 4. Attribute werden beim Spiegeln nicht mitgenommen ==="
cp pool/src/fuse/server.rs /tmp/server.bak
perl -0pi -e 's{            let \(_, failed\) = backing::copy_xattrs\(&from, &root, ancestor\);}{            let (_, failed) = (0, 0);}' pool/src/fuse/server.rs
run
cp /tmp/server.bak pool/src/fuse/server.rs

echo "=== wiederhergestellt ==="; run

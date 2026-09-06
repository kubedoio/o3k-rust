# P14.3 — Image and Public-Key Transfer

Implement only authenticated image-byte transfer and keypair public-key import
for the bounded profile. Use canonical O3K image/keypair APIs, verify size and
digest, preserve no private material, and make transfer resumable or safely
restartable. Test corrupt/truncated bytes, timeout/unknown outcome, duplicate
create, cross-scope access, and exact compensation.

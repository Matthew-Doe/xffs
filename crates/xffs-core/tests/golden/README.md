`clean-control.bin` is a 4096-byte control at block 1, sequence 1, clean,
count/checksum zero, used payload 24. Generated independently with Python
`struct.pack_into` at the specification offsets and a bit-at-a-time reflected
CRC32C polynomial 0x82f63b78 (initial/final XOR 0xffffffff). Every other byte
is zero. The codec tests compare expected fields and flip every byte in turn.

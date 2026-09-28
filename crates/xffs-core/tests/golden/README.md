`clean-control.bin` is a 4096-byte control at block 1, sequence 1, clean,
count/checksum zero, used payload 24. Generated independently with Python
`struct.pack_into` at the specification offsets and a bit-at-a-time reflected
CRC32C polynomial 0x82f63b78 (initial/final XOR 0xffffffff). Every other byte
is zero. The codec tests compare expected fields and flip every byte in turn.

`root-table.bin` is independently encoded with the same Python packing/bitwise
CRC procedure: type 5, physical block 516, payload 3840; slot zero is root (0,1),
linked directory, parent (0,1), timestamps 1700000000, size/allocation/count zero.
All remaining inode slots and padding are zero.

`revision-two-table.bin` is independently packed with Python `struct.pack_into`
and the same bitwise CRC polynomial (without calling the Rust codecs). Header:
revision 2, table at block 516, used payload 3840. Slot 0 is the revision 1 root
above. Slot 1 is linked file (1,9), parent (0,1), size 1, allocated 2, one inline
extent (logical 0, physical 600, length 2), times 1700000000/1/2, cleanup bound
8192 and access time 123. Slot 2 is free (2,u64::MAX), permanently retired.
Every other slot/reserved byte is zero. This is a codec vector, not a complete
filesystem (the linked file needs a directory entry in a complete image).

# Live capture: WoT 0.6.5 login request (real client)

Captured 2026-11-11 from the retail 0.6.5 client (`WorldOfTanks.exe`) pointed at
`127.0.0.1:20016` by `tools/udp_log.py` (passive UDP listener). 10 identical-size
datagrams, one per ~1 s — the client retries the login request with a fresh reply id.

Raw vector: `.scratch/research/protocol-scout/login-request.bin`
All retries: `.scratch/research/protocol-scout/login-request-retries.hx`

## Decoded

```
u16 LE flags          = 0x0001  (HAS_REQUESTS)
u8     element id     = 0x00    (LoginInterface::login, VARIABLE 2)
u16 LE length         = 264     (ARGS ONLY — request header not counted; see note)
i32 LE reply_id       = 47868   (differs on every retry)
u16 LE next_request   = 0
--- args (264 B) ---
u32 LE LOGIN_VERSION  = 57      (BW 2.0.1 ships 56; WoT client is one revision ahead)
u32 LE unknown        = 0x2eca0334 (constant across retries of one attempt; purpose TBD)
256 B  RSA-OAEP-SHA1  = LogOnParams ciphertext (decrypts with our private key)
--- footer ---
u16 LE first_request_offset = 2
```

Plaintext of the RSA block (60 B), decrypted with `.scratch/wot-keys/loginapp_priv.pem`:

```
01                        flags = HAS_DIGEST
0d "test@test.com"        username (packed-int length + bytes)
07 "test123"              password
10 9a01a49e7dbbbb0d41340cb57e3531ff   encryptionKey (16 raw bytes)
9d4e36229ab1e0c34a4bd61f8d760ffd      MD5 digest (16 B)
33 42 00 00               nonce (u32 LE = 0x4233)
```

The `encryptionKey` is the Blowfish key the client uses for the BaseApp channel; the
LoginApp must pass it to the BaseApp (our session handoff file).

## Expected LoginApp reply (validated shape)

```
u16 LE flags = 0x0000
u8     id    = 0xFF (REPLY)
u32 LE len   = 4 + payload_len
i32 LE reply_id (echo)
payload:
  u8 status = 1 (LOGGED_ON)
  16 B Blowfish-XOR-chain( 10-byte LoginReplyRecord )
     u32 raw ip   127.0.0.1 -> 7f 00 00 01
     u16 raw port 20017     -> 31 4e
     u16 salt     0
     u32 LE session_key
```

Blowfish: ECB, `cipher_block = BF_encrypt(plain_block XOR previous_plain_block)`, IV=0,
zero-pad to 16 bytes; key = client's `encryptionKey`.

## Length-field note

`277 = 2 (flags) + 1 (id) + 2 (length) + 4 (reply id) + 2 (next request) + 264 (args) + 2 (footer)`
so the element `length` field counts the **args only**. `docs/research/bw201-login-wire.md` §1.3
states it includes the 6-byte request header; the capture is ground truth for the 0.6.5 client.

The client is a live integration test: `tools/loginapp_proto.py` implements this end-to-end and
answered the real client (then the client proceeds to the BaseApp hop).

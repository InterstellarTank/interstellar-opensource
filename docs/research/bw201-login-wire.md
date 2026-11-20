# BigWorld 2.0.1 LoginApp wire spec (for WoT 0.6.5 clients)

Status: authoritative for the loginapp hop. Derived from the BigWorld 2.0.1 source drop
(clone: `.scratch/research/protocol-scout/repos/bw201`, upstream `v2v3v4/BigWorld-Engine-2.0.1`)
and cross-checked against the 0.6.5 client binary (`WorldOfTanks.exe`) and decompiled 0.6.5
Python. File paths below are relative to that clone's `src/`.

**Why this matches 0.6.5:** the 0.6.5 client binary embeds BigWorld 2.0.1's
`lib/connection` class names (`LoginHandler`, `LoginAppLoginRequest`, `BaseAppLoginRequest`,
`RetryingRequest`, `RSAStreamEncoder` in the exe string table) and its
`BaseAppExtInterface` / `ClientInterface` message-name tables appear byte-for-byte in
BigWorld 2.0.1's declaration order (see `.scratch/research/protocol-scout/wot065-exe-strings.txt`,
indexes 24382-24451). Build path in the exe: `d:\WoT\game_releases\beta_6.5\...`; client
Python compiles 2011-05-26. Genuine deltas vs BigWorld 2.0.1 would be WG's own additions
later in the session; none are needed for login.

> **CORRECTION to an earlier assumption:** the login request payload is
> `u32 LOGIN_VERSION` **immediately followed by the (optionally RSA-encrypted)
> LogOnParams bytes**. There is **no `bool encrypted` byte** in BigWorld 2.0.1. That bool
> exists only in the modern (2020s Core-engine) protocol that `wg-toolkit` implements
> (`wg-toolkit/src/app/login/element.rs`). The loginapp decides "encrypted or not" by
> trying the private-key decrypt and falling back to plaintext
> (`server/loginapp/loginapp.cpp`, `LoginApp::login`).

---

## 1. Transport: Mercury packet format (BW 2.0.1, LOGIN_VERSION 56)

Source: `lib/network/packet.hpp`, `lib/network/bundle.cpp`, `lib/network/network_interface.cpp`,
`lib/network/interface_element.cpp`, `lib/network/basictypes.cpp`, `lib/cstdmf/binary_stream.ipp`.

* UDP. **No 4-byte prefix** — unlike the modern WoT/Core protocol implemented by
  `wg-toolkit` (`wg-toolkit/src/net/packet.rs` documents a 4-byte prefix; that was added
  years later). A BW 2.0.1 packet starts at offset 0 with a 2-byte flags word.
* Byte order: **little-endian**. BigWorld's `BW_HTONS`/`BW_HTONL` are endian-*normalising*
  helpers that are the identity on x86 (`lib/cstdmf/binary_stream.ipp` lines 79/171/200:
  `#define BW_HTONS( x ) x` on little-endian builds). The header comment in
  `binary_stream.ipp` states explicitly: "the BigWorld network byte order is little-endian".
* Packet max total size 1472 bytes (`PACKET_MAX_SIZE`, `packet.hpp:19`).

### 1.1 Packet header

```
u16 flags          # Packet::Flags = uint16, little-endian
```
Flag bits (`lib/network/packet.hpp:70-80`):
`HAS_REQUESTS=0x0001, HAS_PIGGYBACKS=0x0002, HAS_ACKS=0x0004, ON_CHANNEL=0x0008,
IS_RELIABLE=0x0010, IS_FRAGMENT=0x0020, HAS_SEQUENCE_NUMBER=0x0040, INDEXED_CHANNEL=0x0080,
HAS_CHECKSUM=0x0100, CREATE_CHANNEL=0x0200, HAS_CUMULATIVE_ACK=0x0400`.
Checksums are only on when the sender is configured for them
(`NetworkInterface::shouldUseChecksums_`, default **false**, `network_interface.cpp:92`);
the WoT client runs without them. If `HAS_CHECKSUM` is set, a `u32 XOR-checksum` footer is
the last 4 bytes of the packet (`Packet::writeChecksum`/`validateChecksum`, `packet.cpp`).

### 1.2 Elements inside the body

Each element: `u8 id`, then length, then payload.
* `FIXED_LENGTH_MESSAGE` (`INTERFACE` macro `MERCURY_FIXED_MESSAGE`): no length field; body
  length is the declared constant.
* `VARIABLE_LENGTH_MESSAGE`: `lengthParam` bytes of length, **little-endian**, value = number
  of payload bytes *after* the length field (it includes the 6-byte request header when the
  element is a request). Widths used by BW: 1, 2, 3 (BW_PACK3), 4 bytes
  (`InterfaceElement::compressLength/expandLength`, `interface_element.cpp`).
  Special case: if the value does not fit, the length field is all-0xFF and a `u32`
  little-endian real length sits at the start of the payload; for our traffic never needed.
* `REPLY` element is a pseudo element: id **0xFF**, VARIABLE with 4-byte length
  (`const InterfaceElement::REPLY("Reply", REPLY_MESSAGE_IDENTIFIER, VARIABLE_LENGTH_MESSAGE, 4)`,
  `interface_element.cpp`; `REPLY_MESSAGE_IDENTIFIER = 0xFF` in `interface_element.hpp`).
* Entity messages are ids 128..254 dispatched by entity-def exposed ids; 0xFF is reserved for
  replies (`interface_element.hpp` comment). Not used on the loginapp hop.

Element header size: `1 + lengthParam` for variable, `1` for fixed
(`InterfaceElement::headerSize()`).

### 1.3 Requests (client -> loginapp)

`RetryingRequest::send()` (`lib/connection/retrying_request.cpp`) creates a Mercury **request**:

```
packet body:
  u8  id            # LoginInterface::login = 0
  u16 length        # LE, = 6 + args_len
  i32 reply_id      # LE, generated client-side, MUST be echoed in the reply
  u16 next_request_offset   # LE; 0 when this is the last request in the packet
  ...args...
packet footers (flags 0x0001 HAS_REQUESTS):
  u16 first_request_offset  # LE, offset of the first request element from packet start (=2)
```
* `retrying_request.cpp:send()`: `bundle.startRequest(ie_, this, NULL, timeoutPeriod, Mercury::RELIABLE_NO)`
  — **no Mercury reliability/sequence**, retries are done at the login layer (new login
  request every `retryPeriod`). A re-send has a **new reply_id**; the loginapp caches recent
  attempts to answer duplicates (`LoginApp::handleResentPendingAttempt`,
  `server/loginapp/loginapp.cpp`).
* After `newMessage()`, `Bundle::startRequest` reserves `sizeof(ReplyID)+sizeof(Packet::Offset)`
  = 4+2 bytes *after* the element header and before the user args
  (`lib/network/bundle.cpp:329-342`). `RequestManager` later writes the reply id there
  (`*(order->pReplyID) = BW_HTONL( replyID )`, `lib/network/request_manager.cpp:138-145`).
  `Packet::addRequest` writes 0 into the `next_request_offset` slot of the last request and
  links earlier ones (`lib/network/packet.cpp`).
* Reply id values start around `(timestamp()%100000)+10101` (`request_manager.cpp:34`).

A client login request packet is therefore (flags=0x0001):

```
01 00                          flags = HAS_REQUESTS (LE)
00                             element id 0 = login
LL LL                          length = 6 + args_len (LE)
RR RR RR RR                    reply_id (LE i32)
00 00                          next_request_offset = 0
<args, see §2>                 
FO FO                          first_request_offset = 2 (LE u16)
```

### 1.4 Replies (loginapp -> client)

`LoginApp::sendSuccess/sendFailure` (`server/loginapp/loginapp.cpp`) use
`bundle.startReply( header.replyID, Mercury::RELIABLE_NO )`:

```
packet body:
  u8   id           # 0xFF (REPLY)
  u32  length       # LE = 4 + payload_len
  i32  reply_id     # LE, echo of the request's reply id
  <payload>
```
The packet has **no flags and no footers** (flags word `00 00`) because the reply is
unreliable/off-channel and checksums are off. Nothing is sent back to ack the client's
request — the client simply retries if the reply is lost.

---

## 2. Login request payload

`LoginAppLoginRequest::addRequestArgs` (`lib/connection/loginapp_login_request.cpp`):

```cpp
bundle << LOGIN_VERSION;                          // uint32, 56 in BW 2.0.1
if (!pParams->addToStream( bundle, LogOnParams::HAS_ALL,
        pParent_->pServerConnection()->pLogOnParamsEncoder() )) ...
```

So the payload is exactly:

```
u32 LOGIN_VERSION          # LE; BW 2.0.1 ships 56 (lib/connection/login_interface.hpp)
<LogOnParams>              # RSA-encrypted if a public key was configured (WoT always does)
```

There is **no plaintext "encrypted" flag**. The loginapp (`LoginApp::login`) reads the u32,
then tries `LogOnParams::readFromStream(attempt, pEncoder)` with the server's
`RSAStreamEncoder(keyIsPrivate=true)`; only if the server allows unencrypted logins does it
retry without the encoder (`loginapp.cpp`, the `do { ... } while(false)` loop).

### 2.1 LogOnParams plaintext (`lib/connection/log_on_params.cpp:addToStreamInternal`)

```cpp
data << flags << username_ << password_ << encryptionKey_;
if (flags & HAS_DIGEST) data << digest_;          // MD5::Digest = 16 raw bytes
data << nonce_;                                   // uint32
```
* `flags` is `uint8`; `HAS_DIGEST = 0x1` (also `HAS_ALL = 0x1`, `PASS_THRU = 0xFF` is an
  internal sentinel, never on the wire). The 0.6.5 client fills digest, so on the wire
  `flags == 0x01`.
* `std::string` on a BigWorld binary stream is **packed-int length + raw bytes**, *not* a
  u32/u16 length (`lib/cstdmf/binary_stream.hpp:168,353` / `:232-250`):

```cpp
inline void BinaryOStream::writePackedInt( int value )
{
    if (value >= 255) { (*this) << (uint8)0xff; BW_PACK3( reserve(3), value ); }
    else              { (*this) << (uint8)value; }
}
inline void BinaryOStream::appendString( const char *data, int length )
{ this->writePackedInt( length ); this->addBlob( data, length ); }
```
  So: `len < 255` -> `u8 len`; `len >= 255` -> `0xFF` then 3 bytes **little-endian**
  (BW_PACK3 writes bytes low..high). Strings carry no NUL terminator.
* `nonce` is a `uint32` (little-endian), seeded with `rand()`; only used by the loginapp to
  detect duplicate login attempts (`operator==` on LogOnParams). Echo nothing; the reply has
  no nonce.
* `digest` is `MD5::Digest` = 16 raw bytes, i.e. 1-byte packed-int is *not* used, it is
  streamed as `insertRaw` (raw 16). It is the MD5 of the client/server protocol data
  (entity definitions), set from `ServerConnection::digest()`; the loginapp forwards it to
  the dbmgr and may reject with `LOGIN_REJECTED_BAD_DIGEST`. **A private server should ignore
  its value** (parse 16 bytes if `flags&1`, never reject). The client only reports the
  status the server returns; it does not check the server's digest itself.

### 2.2 Example plaintext (2048-bit RSA, single block)

```
01                             flags = HAS_DIGEST
04 74 65 73 74                 username "test"   (packed-int len 4 = 0x04)
04 74 65 73 74                 password "test"
10 3f 91 ... (16 random)       encryptionKey    (packed-int len 16 = 0x10; raw bytes)
<16 bytes MD5>                 digest
a1 b2 c3 d4                    nonce (LE u32)
```
Length = 1+5+5+17+16+4 = 48 bytes. For RSA-2048 this is one 256-byte cipher block.

### 2.3 RSA framing and padding (`lib/network/public_key_cipher.cpp`)

* Key file: PEM. Client loads the **public** key (`PEM_read_bio_RSA_PUBKEY`) from
  `publicKeyPath` (the 0.6.5 client passes `loginapp_wot.pubkey` from `scripts_config.xml`;
  file `res/loginapp_wot.pubkey` = 451-byte PEM, RSA 2048).
* Padding: **PKCS#1 OAEP with SHA-1**. `public_key_cipher.cpp`: `#define RSA_PADDING 41` with
  the comment "RSA_PKCS1_OAEP_PADDING padding mode is used"; clear-chunk size is
  `rsaModulusSize - RSA_PADDING - 1` = 256-41-1 = **214** bytes; cipher chunk = 256 bytes.
* The plaintext is split into successive 214-byte chunks, each encrypted separately with
  OAEP-SHA1, ciphertexts simply concatenated. On the wire: `ceil(plain_len/214) * 256` bytes.
  There is **no per-chunk length prefix**; the element length delimits the blob. For a
  48-byte LogOnParams that is exactly 256 bytes.
* Decrypt: for each 256-byte chunk `RSA_private_decrypt(256, in, out, key, RSA_PKCS1_OAEP_PADDING)`,
  concatenate plaintext (mirror of `PublicKeyCipher::decrypt`).
* Rust crates: `rsa` with `Oaep::new::<Sha1>()` reproduces this exactly (that is what
  `wg-toolkit/src/net/filter/rsa.rs` already uses: clear chunk cap `key.size() - 41 - 1`,
  `Oaep::new::<Sha1>()`).

> 0.6.5-specific note: `res/loginapp_wot.pubkey` is **plain PEM text** and is *not* inside a
> packed resource, so replacing it with our own RSA-2048 public key is a plain file swap
> (see `docs/research/protocol-scout.md` §deployment). The matching private key then lives
> only on our Rust server.

---

## 3. LoginApp reply payload

`LoginApp::sendSuccess` / `sendFailure` (`server/loginapp/loginapp.cpp`), status enum from
`lib/connection/log_on_status.hpp`.

### 3.1 Success

```
payload:
  u8 status = 1                      # LogOnStatus::LOGGED_ON == 1 (NOT 0)
  <Blowfish-encrypted LoginReplyRecord>
```
`LoginReplyRecord` = `{ Mercury::Address serverAddr; uint32 sessionKey; }`
(`lib/connection/login_reply_record.hpp`).

Plaintext of that record (10 bytes), then stream-encrypted:

```
u32 ip        # raw host-order u32, on x86 LE; e.g. 127.0.0.1 = 7f 00 00 01
u16 port      # raw host-order u16, LE; e.g. 20017 = 51 4e
u16 salt      # streamed via operator<< (BW_HTONS identity -> LE); must be 0 for a
              # client-facing BaseApp address (Address::salt is only for entity ids)
u32 session_key   # LE; the login key to present to the BaseApp
```
`Address` streaming (`lib/network/basictypes.cpp:208`): "we don't use the streaming operators
because they will do endian conversions... These values need to be in the same byte order on
both systems so we just use the raw methods": `os.insertRaw(a.ip); os.insertRaw(a.port); os << a.salt;`
So ip/port are raw little-endian on our x86 target; salt is a normal u16 stream.
`Address` struct: `uint32 ip; uint16 port; uint16 salt;` (`lib/network/basictypes.hpp:242`).

### 3.2 Blowfish encryption of the reply record (`lib/network/encryption_filter.cpp`)

`LoginApp::sendSuccess` builds `Mercury::EncryptionFilter filter(encryptionKey)` where
`encryptionKey` is the **client's session key string taken from LogOnParams** (so the client
already knows it), then:

```cpp
MemoryOStream clearText; clearText << replyRecord;   // 10 bytes
filter.encryptStream( clearText, b );
```
* `encryptStream` zero-pads the clear text to a multiple of **8** bytes
  (`BLOCK_SIZE = 64/8`), then encrypts with `Blowfish` (OpenSSL `BF_ecb_encrypt`), where each
  block is XORed with the **previous plaintext block** before encryption (not with the
  previous ciphertext; the "replay attack" XOR stage added in BW protocol version 49):
  `dest[i] = src[i] ^ src[i-8]` then `BF_ecb_encrypt`, `pPrevBlock = src+i`
  (`EncryptionFilter::encrypt`, `encryption_filter.cpp`).
* Key length from the client: `EncryptionFilter(int keySize)` default
  `DEFAULT_KEY_SIZE = 128/8 = 16` bytes; valid range 4..56
  (`lib/network/encryption_filter.hpp:44-49`). The client builds its random
  `pFilter_ = new EncryptionFilter()` in `ServerConnection`'s constructor and sends
  `pFilter_->key()` as `LogOnParams::encryptionKey` (`server_connection.cpp`, `logOnBegin`).
  The key is a raw byte string (usually 16 random bytes), length-prefixed as in §2.1.
* Hence the success reply body is 16 encrypted bytes for a 10-byte record
  (`ceil(10/8)*8 = 16`). No checksum/magic footer in stream mode.
* Decrypt in Rust: read 8-byte blocks; `plain = BF_decrypt(cipher) XOR prev_plain`, prev starts 0.
  (`wg-toolkit/src/net/filter/blowfish.rs` implements exactly this both ways — reuse it.)

### 3.3 Failure

```
payload:
  u8 status          # LogOnStatus value
  std::string message  # packed-int length + raw bytes (may be empty)
```
Unencrypted. Status enum (`lib/connection/log_on_status.hpp`):
```
NOT_SET=0, LOGGED_ON=1, CONNECTION_FAILED=2, DNS_LOOKUP_FAILED=3, UNKNOWN_ERROR=4,
CANCELLED=5, ALREADY_ONLINE_LOCALLY=6, PUBLIC_KEY_LOOKUP_FAILED=7, LAST_CLIENT_SIDE_VALUE=63,
LOGIN_MALFORMED_REQUEST=64, LOGIN_BAD_PROTOCOL_VERSION=65,
LOGIN_REJECTED_NO_SUCH_USER=67, LOGIN_REJECTED_INVALID_PASSWORD=68,
LOGIN_REJECTED_ALREADY_LOGGED_IN=69, LOGIN_REJECTED_BAD_DIGEST=70,
LOGIN_REJECTED_DB_GENERAL_FAILURE=71, LOGIN_REJECTED_DB_NOT_READY=72,
LOGIN_REJECTED_ILLEGAL_CHARACTERS=73, LOGIN_REJECTED_SERVER_NOT_READY=74,
LOGIN_REJECTED_UPDATER_NOT_READY=75, LOGIN_REJECTED_NO_BASEAPPS=76,
LOGIN_REJECTED_BASEAPP_OVERLOAD=77, LOGIN_REJECTED_CELLAPP_OVERLOAD=78,
LOGIN_REJECTED_BASEAPP_TIMEOUT=79, LOGIN_REJECTED_BASEAPPMGR_TIMEOUT=80,
LOGIN_REJECTED_DBMGR_OVERLOAD=81, LOGIN_REJECTED_LOGINS_NOT_ALLOWED=82,
LOGIN_REJECTED_RATE_LIMITED=83, LOGIN_CUSTOM_DEFINED_ERROR=254
```
(The client's Python maps status *names* to UI strings, see `decompiled/client/gui/Scaleform/Login.py`
`__logOnFailedHandlers`; the C++ `ConnectionControl` emits stage/status callbacks, see
`client/connection_control.cpp` docs and `decompiled/client/ConnectionManager.py:connectionWatcher`.)

**The loginapp does not validate LOGIN_VERSION against the client's value on the client side;
only the loginapp does (server-side).** Our server should accept whatever the 0.6.5 client sends
(first u32) and log it; no version-dependent framing exists in this range (all BW >= 43 uses
the 2-byte flags header; >=49 has the Blowfish XOR stage; >=52 cumulative ACKs; the 0.6.5 client
predates none of these). Emit the version back nowhere — there is no version field in the reply.

---

## 4. What happens right after LOGGED_ON (BaseApp hop)

Client side, `lib/connection/login_handler.cpp:onLoginReply`:

1. status byte == LOGGED_ON -> decrypt `LoginReplyRecord` with its `pFilter_` (Blowfish, §3.2),
   set `baseAppAddr_ = replyRecord.serverAddr`, `sessionKey = replyRecord.sessionKey`.
2. `sendBaseAppLogin()` -> `BaseAppLoginRequest` (`lib/connection/baseapp_login_request.cpp`):
   * creates a **new** `Mercury::NetworkInterface` and a `Mercury::Channel` to the BaseApp,
     with `pServConn->pFilter()` (the same Blowfish filter/session key), channel is
     `EXTERNAL`, not regular yet (`isLocalRegular(false); isRemoteRegular(false)`).
   * sends a **request** `BaseAppExtInterface::baseAppLogin` (id 0, `BW_STREAM_MSG_EX` =>
     VARIABLE-2 element) with args `uint32 sessionKey; int32 attempt` (8 bytes; attempt 0
     first, retried up to `MAX_BASEAPP_LOGIN_ATTEMPTS = 10`, each retry a new request object
     and new interface/channel).
   * The BaseApp's reply is a Mercury reply: `u32 sessionKey` (a **new** key, chosen by the
     server; `LoginHandler::onBaseAppReply` stores it as `sessionKey_`).
3. Once the reply is in, the client adopts that Channel and "primes" every outgoing bundle with
   `BaseAppExtInterface::authenticate` — id 1, FIXED 4 — carrying the new session key as a
   `RELIABLE_PASSENGER` message (`server_connection.cpp:primeBundle`).
   The server must send `ClientInterface::authenticate` (id 0, FIXED 4, `u32 key`) with the
   same key; the client compares it (`ServerConnection::authenticate`: `args.key != sessionKey_`
   -> error).
4. After that the server drives the session (createBasePlayer etc.); see
   `docs/research/bw201-baseapp-wire.md`.

Channel-creation caveat: the first BaseApp packet is sent on a *new* channel
(`FLAG_CREATE_CHANNEL` handshake, Channel::writeFlags/writeFooter in
`lib/network/channel.cpp`), and the BaseApp's answer may need to travel on the
then-established reliable channel. Implementations should accept the first packet that both
creates and carries the request and answer off-channel with a Mercury reply (the client's
`BaseAppLoginRequest` is an off-channel-style `RetryingRequest`; it adopts the channel only on
reply). If the client refuses, inspect `FLAG_CREATE_CHANNEL`/`FLAG_INDEXED_CHANNEL` handling in
`lib/network/channel.cpp` and `packet_receiver.cpp`. This is the one place where a live capture
is strongly recommended to confirm channel establishment details before trusting source alone.

---

## 5. Reference implementation checklist / test vectors

Unit-testable without the game:

1. **packed-int**: `pack_int(4) == [0x04]`; `pack_int(254) == [0xfe]`;
   `pack_int(255) == [0xff, 0xff, 0x00, 0x00]`; `pack_int(0x010203) == [0xff, 0x03, 0x02, 0x01]`
   (from `writePackedInt`/`BW_PACK3`).
2. **LogOnParams** round-trip: build `flags=1, username="test", password="test",
   key=[16 bytes], digest=[16 bytes], nonce=0x12345678`, encode, RSA-OAEP-SHA1 encrypt with a
   generated 2048-bit key, decrypt in the test, compare bytes.
3. **Login request packet** (golden bytes, `.scratch/research/protocol-scout/login-request.bin`
   can be produced by a small test): `01 00 | 00 | len u16 LE | reply_id | 00 00 | u32 56 |
   rsa_blob(256) | first_request_offset=2 u16 LE`. A Rust test should assert the whole layout
   and that the server parser recovers `reply_id`, username, password, encryption key, nonce.
4. **Success reply**: for session key `K`, record `ip=127.0.0.1, port=20017, salt=0,
   sessionKey=S`, encrypt per §3.2, assert 16 bytes and that the client-side decryption
   routine returns the record. `docs/research/bw201-baseapp-wire.md` covers what goes on the
   BaseApp channel afterwards.
5. **Live capture validation** (when the client can be run): replace
   `res/loginapp_wot.pubkey` with our public key, point the client at 127.0.0.1:20016, log the
   first UDP datagram and assert: `flags == 0x0001`, `body[0] == 0x00`,
   `u16 body[1..3] == 6 + args_len`, `u32 payload[0..4]` is the client's LOGIN_VERSION
   (expected 56 if stock BW 2.0.1; accept anything), the rest decrypts with our private key.
   Then answer and continue with the BaseApp handshake.

## 6. Source map

| Topic | File |
|---|---|
| Packet flags, sizes, footers | `lib/network/packet.hpp`, `packet.cpp`, `network_interface.cpp` |
| Element header, REPLY id/length | `lib/network/interface_element.cpp/.hpp`, `interface_macros.hpp` |
| Bundle element writing, requests/replies | `lib/network/bundle.cpp/.hpp` |
| Request ids, retry | `lib/network/request_manager.cpp`, `lib/connection/retrying_request.cpp` |
| LoginInterface (login=0 var2, probe=1 fixed0, LOGIN_VERSION=56) | `lib/connection/login_interface.hpp` |
| Login request | `lib/connection/loginapp_login_request.cpp` |
| LogOnParams | `lib/connection/log_on_params.cpp/.hpp` |
| Public-key encryption | `lib/network/public_key_cipher.cpp`, `lib/connection/rsa_stream_encoder.hpp` |
| Read/write std::string, packed ints | `lib/cstdmf/binary_stream.hpp/.ipp` |
| Address streaming | `lib/network/basictypes.cpp/.hpp` |
| Blowfish filter | `lib/network/encryption_filter.cpp/.hpp` |
| LoginApp server side | `server/loginapp/loginapp.cpp`, `message_handlers.cpp` |
| LoginHandler/BaseAppLoginRequest | `lib/connection/login_handler.cpp`, `baseapp_login_request.cpp` |
| Status enum | `lib/connection/log_on_status.hpp` |
| 0.6.5 client evidence | `.scratch/research/protocol-scout/wot065-exe-strings.txt` (idx 24375-24470, 18908-18929), `.scratch/research/decompiled/client/ConnectionManager.py`, `gui/Scaleform/Login.py` |

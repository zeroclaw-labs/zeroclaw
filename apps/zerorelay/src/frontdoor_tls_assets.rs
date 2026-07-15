pub(crate) const TLS_ENGINE_JS: &str = r#"(() => {
  const CONTENT_CHANGE_CIPHER_SPEC = 20;
  const CONTENT_ALERT = 21;
  const CONTENT_HANDSHAKE = 22;
  const CONTENT_APPLICATION_DATA = 23;
  const TLS_AES_128_GCM_SHA256 = 0x1301;
  const GROUP_SECP256R1 = 0x0017;
  const HASH_LEN = 32;
  const KEY_LEN = 16;
  const IV_LEN = 12;
  const SIGNATURE_ECDSA_SECP256R1_SHA256 = 0x0403;
  const OID_ECDSA_SHA256 = '1.2.840.10045.4.3.2';
  const OID_EC_PUBLIC_KEY = '1.2.840.10045.2.1';
  const OID_PRIME256V1 = '1.2.840.10045.3.1.7';
  const OID_BASIC_CONSTRAINTS = '2.5.29.19';
  const OID_SUBJECT_ALT_NAME = '2.5.29.17';
  const OID_EXTENDED_KEY_USAGE = '2.5.29.37';
  const OID_SERVER_AUTH = '1.3.6.1.5.5.7.3.1';
  const textEncoder = new TextEncoder();
  const textDecoder = new TextDecoder();

  class BrowserTls13Client {
    constructor(transport) {
      this.transport = transport;
      this.recv = new Uint8Array(0);
      this.handshakeBytes = [];
      this.handshakeRead = new Uint8Array(0);
      this.clientHandshake = null;
      this.serverHandshake = null;
      this.clientApp = null;
      this.serverApp = null;
      this.serverCertDer = null;
    }

    static async connect(transport, options = {}) {
      const client = new BrowserTls13Client(transport);
      await client.handshake(options);
      return client;
    }

    async handshake(options) {
      const ecdh = await crypto.subtle.generateKey(
        { name: 'ECDH', namedCurve: 'P-256' },
        false,
        ['deriveBits']
      );
      const keyShare = new Uint8Array(await crypto.subtle.exportKey('raw', ecdh.publicKey));
      const clientHello = await this.clientHello(options.serverName || '127.0.0.1', keyShare);
      this.rememberHandshake(clientHello);
      await this.writePlainRecord(CONTENT_HANDSHAKE, clientHello);

      const serverHello = await this.readHandshake();
      const serverKeyShare = parseServerHello(serverHello);
      this.rememberHandshake(serverHello);
      const serverPub = await crypto.subtle.importKey(
        'raw',
        serverKeyShare,
        { name: 'ECDH', namedCurve: 'P-256' },
        false,
        []
      );
      const sharedBits = new Uint8Array(await crypto.subtle.deriveBits(
        { name: 'ECDH', public: serverPub },
        ecdh.privateKey,
        256
      ));
      await this.installHandshakeKeys(sharedBits);

      let certificateRequested = false;
      for (;;) {
        const msg = await this.readHandshake();
        const kind = msg[0];
        if (kind === 8) {
          this.rememberHandshake(msg);
        } else if (kind === 13) {
          certificateRequested = true;
          this.rememberHandshake(msg);
        } else if (kind === 11) {
          this.serverCertDer = parseCertificateMessage(msg);
          this.rememberHandshake(msg);
        } else if (kind === 15) {
          await this.verifyServerCertificateVerify(msg);
          this.rememberHandshake(msg);
        } else if (kind === 20) {
          await this.verifyServerFinished(msg.slice(4));
          this.rememberHandshake(msg);
          break;
        } else {
          throw new Error(`unexpected TLS handshake message ${kind}`);
        }
      }

      if (options.caChainPem) {
        await verifyServerCertificateChain(this.serverCertDer, options.caChainPem, options.serverName || '127.0.0.1');
      }

      await this.installApplicationKeys();

      if (certificateRequested) {
        const cert = await this.clientCertificateMessage(options.clientCertificatePem);
        this.rememberHandshake(cert);
        await this.writeEncryptedRecord(this.clientHandshake, CONTENT_HANDSHAKE, cert);

        const verify = await this.clientCertificateVerify(options.clientSigningKey);
        this.rememberHandshake(verify);
        await this.writeEncryptedRecord(this.clientHandshake, CONTENT_HANDSHAKE, verify);
      }

      const finished = await this.clientFinishedMessage();
      this.rememberHandshake(finished);
      await this.writeEncryptedRecord(this.clientHandshake, CONTENT_HANDSHAKE, finished);
    }

    async clientHello(serverName, keyShare) {
      const legacyVersion = bytes(0x03, 0x03);
      const random = crypto.getRandomValues(new Uint8Array(32));
      const sessionId = crypto.getRandomValues(new Uint8Array(32));
      const cipherSuites = vec16(u16(TLS_AES_128_GCM_SHA256));
      const compressionMethods = vec8(bytes(0x00));
      const extensions = [];
      if (serverName && !isIpAddress(serverName)) {
        const host = textEncoder.encode(serverName);
        extensions.push(extension(0x0000, vec16(concat(
          bytes(0x00),
          u16(host.length),
          host
        ))));
      }
      extensions.push(extension(0x002b, vec8(u16(0x0304))));
      extensions.push(extension(0x000a, vec16(u16(GROUP_SECP256R1))));
      extensions.push(extension(0x000d, vec16(concat(
        u16(0x0403),
        u16(0x0804),
        u16(0x0805),
        u16(0x0806)
      ))));
      extensions.push(extension(0x0033, vec16(concat(
        u16(GROUP_SECP256R1),
        vec16(keyShare)
      ))));

      const body = concat(
        legacyVersion,
        random,
        vec8(sessionId),
        cipherSuites,
        compressionMethods,
        vec16(concat(...extensions))
      );
      return handshakeMessage(1, body);
    }

    async installHandshakeKeys(sharedBits) {
      const zeros = new Uint8Array(HASH_LEN);
      const emptyHash = await sha256(new Uint8Array(0));
      const earlyState = await hkdfExtract(zeros, zeros);
      const derivedEarly = await deriveSecret(earlyState, 'derived', emptyHash);
      this.handshakeState = await hkdfExtract(derivedEarly, sharedBits);
      const transcript = await this.transcriptHash();
      const clientTraffic = await deriveSecret(this.handshakeState, 'c hs traffic', transcript);
      const serverTraffic = await deriveSecret(this.handshakeState, 's hs traffic', transcript);
      this.clientHandshake = await trafficKeys(clientTraffic);
      this.serverHandshake = await trafficKeys(serverTraffic);
      this.clientHandshake.keyingMaterial = clientTraffic;
      this.serverHandshake.keyingMaterial = serverTraffic;
    }

    async installApplicationKeys() {
      const zeros = new Uint8Array(HASH_LEN);
      const emptyHash = await sha256(new Uint8Array(0));
      const derivedHandshake = await deriveSecret(this.handshakeState, 'derived', emptyHash);
      const masterState = await hkdfExtract(derivedHandshake, zeros);
      const transcript = await this.transcriptHash();
      const clientTraffic = await deriveSecret(masterState, 'c ap traffic', transcript);
      const serverTraffic = await deriveSecret(masterState, 's ap traffic', transcript);
      this.clientApp = await trafficKeys(clientTraffic);
      this.serverApp = await trafficKeys(serverTraffic);
    }

    async verifyServerFinished(body) {
      const transcript = await this.transcriptHash();
      const finishedKey = await hkdfExpandLabel(
        this.serverHandshake.keyingMaterial,
        'finished',
        new Uint8Array(0),
        HASH_LEN
      );
      const expected = await hmac(finishedKey, transcript);
      if (!constantTimeEqual(expected, body)) {
        throw new Error('TLS server Finished verification failed');
      }
    }

    async verifyServerCertificateVerify(msg) {
      if (!this.serverCertDer) {
        throw new Error('server CertificateVerify arrived before Certificate');
      }
      const r = new Reader(msg.slice(4));
      const scheme = r.u16();
      if (scheme !== SIGNATURE_ECDSA_SECP256R1_SHA256) {
        throw new Error(`unsupported server CertificateVerify scheme 0x${scheme.toString(16)}`);
      }
      const signature = r.bytes(r.u16());
      if (r.remaining() !== 0) {
        throw new Error('server CertificateVerify has trailing bytes');
      }
      const transcript = await this.transcriptHash();
      const signed = concat(
        repeatedByte(0x20, 64),
        textEncoder.encode('TLS 1.3, server CertificateVerify'),
        bytes(0x00),
        transcript
      );
      const cert = parseCertificate(this.serverCertDer);
      const publicKey = await importEcdsaPublicKey(cert.spkiDer);
      const ok = await crypto.subtle.verify(
        { name: 'ECDSA', hash: 'SHA-256' },
        publicKey,
        ecdsaDerSignatureToRaw(signature),
        signed
      );
      if (!ok) {
        throw new Error('TLS server CertificateVerify signature failed');
      }
    }

    async clientFinishedMessage() {
      const transcript = await this.transcriptHash();
      const finishedKey = await hkdfExpandLabel(
        this.clientHandshake.keyingMaterial,
        'finished',
        new Uint8Array(0),
        HASH_LEN
      );
      return handshakeMessage(20, await hmac(finishedKey, transcript));
    }

    async clientCertificateMessage(certPem) {
      const certs = certPem ? pemBlocks(certPem, 'CERTIFICATE') : [];
      const entries = certs.map((der) => concat(vec24(der), u16(0)));
      return handshakeMessage(11, concat(vec8(new Uint8Array(0)), vec24(concat(...entries))));
    }

    async clientCertificateVerify(signingKey) {
      if (!signingKey) {
        throw new Error('daemon requested a client certificate but no private key is available');
      }
      const transcript = await this.transcriptHash();
      const prefix = new Uint8Array(64);
      prefix.fill(0x20);
      const context = textEncoder.encode('TLS 1.3, client CertificateVerify');
      const signed = concat(prefix, context, bytes(0x00), transcript);
      const rawSig = new Uint8Array(await crypto.subtle.sign(
        { name: 'ECDSA', hash: 'SHA-256' },
        signingKey,
        signed
      ));
      return handshakeMessage(15, concat(u16(0x0403), vec16(ecdsaRawSignatureToDer(rawSig))));
    }

    async postJson(path, body, host = '127.0.0.1') {
      const json = JSON.stringify(body);
      const request = textEncoder.encode(
        `POST ${path} HTTP/1.1\r\n` +
        `Host: ${host}\r\n` +
        'Content-Type: application/json\r\n' +
        `Content-Length: ${textEncoder.encode(json).byteLength}\r\n` +
        'Connection: close\r\n' +
        '\r\n' +
        json
      );
      await this.writeApplicationData(request);
      return this.readHttpResponse();
    }

    async getJson(path, host = '127.0.0.1') {
      const request = textEncoder.encode(
        `GET ${path} HTTP/1.1\r\n` +
        `Host: ${host}\r\n` +
        'Connection: close\r\n' +
        '\r\n'
      );
      await this.writeApplicationData(request);
      return this.readHttpResponse();
    }

    async openWebSocket(path = '/', host = '127.0.0.1') {
      const nonce = crypto.getRandomValues(new Uint8Array(16));
      const key = b64(nonce);
      const request = textEncoder.encode(
        `GET ${path} HTTP/1.1\r\n` +
        `Host: ${host}\r\n` +
        'Connection: Upgrade\r\n' +
        'Upgrade: websocket\r\n' +
        'Sec-WebSocket-Version: 13\r\n' +
        `Sec-WebSocket-Key: ${key}\r\n` +
        '\r\n'
      );
      await this.writeApplicationData(request);
      const response = await this.readHttpResponse();
      if (response.status !== 101) {
        throw new Error(`WebSocket upgrade failed: HTTP ${response.status}`);
      }
      return new TlsWebSocket(this);
    }

    async writeApplicationData(data) {
      await this.writeEncryptedRecord(this.clientApp, CONTENT_APPLICATION_DATA, toUint8(data));
    }

    async readApplicationData() {
      for (;;) {
        const content = await this.readContent();
        if (content.type === CONTENT_HANDSHAKE) {
          continue;
        }
        if (content.type === CONTENT_ALERT) {
          throw new Error('TLS alert received');
        }
        if (content.type === CONTENT_APPLICATION_DATA) {
          return content.data;
        }
      }
    }

    async readHttpResponse() {
      let received = new Uint8Array(0);
      for (;;) {
        let content;
        try {
          content = await this.readContent();
        } catch (error) {
          if (received.byteLength > 0) {
            return parseHttpResponse(received);
          }
          throw error;
        }
        if (content.type === CONTENT_HANDSHAKE) {
          continue;
        }
        if (content.type === CONTENT_ALERT) {
          throw new Error('TLS alert received while reading HTTP response');
        }
        if (content.type !== CONTENT_APPLICATION_DATA || content.data.byteLength === 0) {
          continue;
        }
        received = concat(received, content.data);
        const parsed = tryParseHttpResponse(received);
        if (parsed) {
          return parsed;
        }
      }
    }

    async readHandshake() {
      while (this.handshakeRead.byteLength < 4) {
        const content = await this.readContent();
        if (content.type !== CONTENT_HANDSHAKE) {
          throw new Error(`expected TLS handshake, got content type ${content.type}`);
        }
        this.handshakeRead = concat(this.handshakeRead, content.data);
      }
      const len = readU24(this.handshakeRead, 1);
      while (this.handshakeRead.byteLength < 4 + len) {
        const content = await this.readContent();
        if (content.type !== CONTENT_HANDSHAKE) {
          throw new Error(`expected TLS handshake fragment, got content type ${content.type}`);
        }
        this.handshakeRead = concat(this.handshakeRead, content.data);
      }
      const msg = this.handshakeRead.slice(0, 4 + len);
      this.handshakeRead = this.handshakeRead.slice(4 + len);
      return msg;
    }

    async readContent() {
      const record = await this.readRecord();
      if (record.type === CONTENT_CHANGE_CIPHER_SPEC) {
        return this.readContent();
      }
      if (record.type === CONTENT_HANDSHAKE && !this.serverHandshake) {
        return { type: CONTENT_HANDSHAKE, data: record.payload };
      }
      if (record.type !== CONTENT_APPLICATION_DATA) {
        throw new Error(`unexpected TLS record type ${record.type}`);
      }
      const keys = this.serverApp || this.serverHandshake;
      if (!keys) {
        throw new Error('encrypted TLS record arrived before keys were installed');
      }
      const plain = await decryptRecord(keys, record.header, record.payload);
      let end = plain.byteLength - 1;
      while (end >= 0 && plain[end] === 0) {
        end -= 1;
      }
      if (end < 0) {
        throw new Error('TLS inner plaintext omitted content type');
      }
      return { type: plain[end], data: plain.slice(0, end) };
    }

    async writePlainRecord(type, payload) {
      this.transport.write(record(type, payload));
    }

    async writeEncryptedRecord(keys, type, payload) {
      const plain = concat(payload, bytes(type));
      const outerHeader = recordHeader(CONTENT_APPLICATION_DATA, plain.byteLength + 16);
      const cipher = await encryptRecord(keys, outerHeader, plain);
      this.transport.write(concat(outerHeader, cipher));
    }

    async readRecord() {
      const header = await this.readExact(5);
      const type = header[0];
      const len = readU16(header, 3);
      if (len > 0x4000 + 256) {
        throw new Error('TLS record is too large');
      }
      const payload = await this.readExact(len);
      return { type, header, payload };
    }

    async readExact(len) {
      while (this.recv.byteLength < len) {
        const chunk = await this.transport.read();
        if (!chunk) {
          throw new Error('TLS transport closed');
        }
        this.recv = concat(this.recv, toUint8(chunk));
      }
      const out = this.recv.slice(0, len);
      this.recv = this.recv.slice(len);
      return out;
    }

    rememberHandshake(msg) {
      this.handshakeBytes.push(msg);
    }

    async transcriptHash() {
      return sha256(concat(...this.handshakeBytes));
    }
  }

  async function fetchEnrollmentTrust(transport, options) {
    const tls = await BrowserTls13Client.connect(transport, {
      serverName: options.serverName || '127.0.0.1'
    });
    const response = await tls.getJson('/enroll/ca', options.host || '127.0.0.1');
    if (response.status < 200 || response.status >= 300) {
      const detail = response.json?.error || response.bodyText || `HTTP ${response.status}`;
      throw new Error(`enrollment trust fetch failed: ${detail}`);
    }
    return response.json;
  }

  async function enroll(transport, options) {
    if (!options.caChainPem) {
      throw new Error('confirmed daemon CA is required before browser enrollment POST');
    }
    const tls = await BrowserTls13Client.connect(transport, {
      serverName: options.serverName || '127.0.0.1',
      caChainPem: options.caChainPem
    });
    const response = await tls.postJson('/enroll', {
      pairing_code: options.pairingCode,
      csr_pem: options.csrPem
    }, options.host || '127.0.0.1');
    if (response.status < 200 || response.status >= 300) {
      const detail = response.json?.error || response.bodyText || `HTTP ${response.status}`;
      throw new Error(`enrollment failed: ${detail}`);
    }
    return response.json;
  }

  async function connectRpc(transport, options) {
    const tls = await BrowserTls13Client.connect(transport, {
      serverName: options.serverName || '127.0.0.1',
      clientCertificatePem: options.clientCertificatePem,
      clientSigningKey: options.clientSigningKey,
      caChainPem: options.caChainPem
    });
    const ws = await tls.openWebSocket('/', options.host || '127.0.0.1');
    const rpc = new JsonRpcClient(ws);
    await rpc.initialize();
    return rpc;
  }

  class JsonRpcClient {
    constructor(ws) {
      this.ws = ws;
      this.nextId = 1;
      this.pending = new Map();
      this.notificationHandlers = new Set();
      this.closed = false;
      this.readTask = this.readLoop();
    }

    async initialize() {
      await this.call('initialize', {
        protocol_version: 1,
        clientCapabilities: {},
        env: {}
      });
    }

    onNotification(handler) {
      this.notificationHandlers.add(handler);
      return () => this.notificationHandlers.delete(handler);
    }

    async call(method, params = null) {
      if (this.closed) {
        throw new Error('RPC tunnel is closed');
      }
      const id = this.nextId++;
      const request = {
        jsonrpc: '2.0',
        id,
        method
      };
      if (params !== null && params !== undefined) {
        request.params = params;
      }
      const result = new Promise((resolve, reject) => {
        this.pending.set(id, { resolve, reject });
      });
      await this.ws.sendText(JSON.stringify(request));
      return result;
    }

    async readLoop() {
      try {
        for (;;) {
          const text = await this.ws.readText();
          if (text === null) {
            break;
          }
          const msg = JSON.parse(text);
          if (msg.id !== undefined && this.pending.has(msg.id)) {
            const pending = this.pending.get(msg.id);
            this.pending.delete(msg.id);
            if (msg.error) {
              pending.reject(new Error(msg.error.message || 'RPC error'));
            } else {
              pending.resolve(msg.result);
            }
          } else if (msg.id !== undefined && msg.method) {
            await this.ws.sendText(JSON.stringify({
              jsonrpc: '2.0',
              id: msg.id,
              error: {
                code: -32601,
                message: `Unsupported browser RPC request: ${msg.method}`
              }
            }));
          } else if (msg.method) {
            for (const handler of this.notificationHandlers) {
              try {
                handler(msg);
              } catch (_) {}
            }
          }
        }
      } catch (error) {
        for (const pending of this.pending.values()) {
          pending.reject(error);
        }
        this.pending.clear();
      } finally {
        this.closed = true;
      }
    }
  }

  class TlsWebSocket {
    constructor(tls) {
      this.tls = tls;
      this.recv = new Uint8Array(0);
    }

    async sendText(text) {
      await this.sendFrame(0x1, textEncoder.encode(text));
    }

    async sendFrame(opcode, payload) {
      const data = toUint8(payload);
      const mask = crypto.getRandomValues(new Uint8Array(4));
      const head = [];
      head.push(0x80 | opcode);
      if (data.byteLength < 126) {
        head.push(0x80 | data.byteLength);
      } else if (data.byteLength <= 0xffff) {
        head.push(0x80 | 126, (data.byteLength >>> 8) & 0xff, data.byteLength & 0xff);
      } else {
        head.push(0x80 | 127, 0, 0, 0, 0);
        const len = BigInt(data.byteLength);
        head.push(
          Number((len >> 24n) & 0xffn),
          Number((len >> 16n) & 0xffn),
          Number((len >> 8n) & 0xffn),
          Number(len & 0xffn)
        );
      }
      const masked = new Uint8Array(data.byteLength);
      for (let i = 0; i < data.byteLength; i += 1) {
        masked[i] = data[i] ^ mask[i % 4];
      }
      await this.tls.writeApplicationData(concat(new Uint8Array(head), mask, masked));
    }

    async readText() {
      for (;;) {
        const frame = await this.readFrame();
        if (!frame) {
          return null;
        }
        if (frame.opcode === 0x1) {
          return textDecoder.decode(frame.payload);
        }
        if (frame.opcode === 0x8) {
          return null;
        }
        if (frame.opcode === 0x9) {
          await this.sendFrame(0xA, frame.payload);
        }
      }
    }

    async readFrame() {
      while (this.recv.byteLength < 2) {
        const chunk = await this.tls.readApplicationData();
        if (!chunk) {
          return null;
        }
        this.recv = concat(this.recv, chunk);
      }
      const first = this.recv[0];
      const second = this.recv[1];
      const opcode = first & 0x0f;
      const masked = (second & 0x80) !== 0;
      let len = second & 0x7f;
      let offset = 2;
      if (len === 126) {
        while (this.recv.byteLength < offset + 2) {
          this.recv = concat(this.recv, await this.tls.readApplicationData());
        }
        len = readU16(this.recv, offset);
        offset += 2;
      } else if (len === 127) {
        while (this.recv.byteLength < offset + 8) {
          this.recv = concat(this.recv, await this.tls.readApplicationData());
        }
        const high = readU32(this.recv, offset);
        const low = readU32(this.recv, offset + 4);
        if (high !== 0 || low > Number.MAX_SAFE_INTEGER) {
          throw new Error('WebSocket frame is too large');
        }
        len = low;
        offset += 8;
      }
      const maskLen = masked ? 4 : 0;
      while (this.recv.byteLength < offset + maskLen + len) {
        this.recv = concat(this.recv, await this.tls.readApplicationData());
      }
      let mask = null;
      if (masked) {
        mask = this.recv.slice(offset, offset + 4);
        offset += 4;
      }
      const payload = this.recv.slice(offset, offset + len);
      this.recv = this.recv.slice(offset + len);
      if (mask) {
        for (let i = 0; i < payload.byteLength; i += 1) {
          payload[i] ^= mask[i % 4];
        }
      }
      return { opcode, payload };
    }
  }

  function parseServerHello(msg) {
    if (msg[0] !== 2) {
      throw new Error('expected TLS ServerHello');
    }
    const r = new Reader(msg.slice(4));
    r.u16();
    r.bytes(32);
    r.bytes(r.u8());
    const suite = r.u16();
    if (suite !== TLS_AES_128_GCM_SHA256) {
      throw new Error(`unsupported TLS cipher suite 0x${suite.toString(16)}`);
    }
    r.u8();
    const extensions = readExtensions(r.bytes(r.u16()));
    const selected = extensions.get(0x002b);
    if (!selected || selected.byteLength !== 2 || readU16(selected, 0) !== 0x0304) {
      throw new Error('server did not negotiate TLS 1.3');
    }
    const keyShare = extensions.get(0x0033);
    if (!keyShare) {
      throw new Error('server omitted key_share');
    }
    const kr = new Reader(keyShare);
    const group = kr.u16();
    if (group !== GROUP_SECP256R1) {
      throw new Error('server selected an unsupported key share group');
    }
    return kr.bytes(kr.u16());
  }

  function parseCertificateMessage(msg) {
    const r = new Reader(msg.slice(4));
    r.bytes(r.u8());
    const list = new Reader(r.bytes(r.u24()));
    if (list.remaining() === 0) {
      throw new Error('server sent an empty certificate list');
    }
    const cert = list.bytes(list.u24());
    list.bytes(list.u16());
    return cert;
  }

  function tryParseHttpResponse(bytes) {
    const marker = findBytes(bytes, bytesOf('\r\n\r\n'));
    if (marker < 0) {
      return null;
    }
    const head = textDecoder.decode(bytes.slice(0, marker));
    const lines = head.split('\r\n');
    const statusMatch = /^HTTP\/1\.[01] ([0-9]{3})/.exec(lines[0] || '');
    if (!statusMatch) {
      throw new Error('invalid HTTP response from enrollment endpoint');
    }
    const headers = {};
    for (const line of lines.slice(1)) {
      const idx = line.indexOf(':');
      if (idx > 0) {
        headers[line.slice(0, idx).trim().toLowerCase()] = line.slice(idx + 1).trim();
      }
    }
    const bodyStart = marker + 4;
    const contentLength = Number(headers['content-length'] || '0');
    if (!Number.isFinite(contentLength) || contentLength < 0) {
      throw new Error('invalid HTTP Content-Length from enrollment endpoint');
    }
    if (bytes.byteLength < bodyStart + contentLength) {
      return null;
    }
    const bodyBytes = bytes.slice(bodyStart, bodyStart + contentLength);
    const bodyText = textDecoder.decode(bodyBytes);
    let json = null;
    if (bodyText.trim()) {
      json = JSON.parse(bodyText);
    }
    return { status: Number(statusMatch[1]), headers, bodyText, json };
  }

  function parseHttpResponse(bytes) {
    const parsed = tryParseHttpResponse(bytes);
    if (!parsed) {
      throw new Error('incomplete HTTP response from enrollment endpoint');
    }
    return parsed;
  }

  async function trafficKeys(keyingMaterial) {
    return {
      key: await hkdfExpandLabel(keyingMaterial, 'key', new Uint8Array(0), KEY_LEN),
      iv: await hkdfExpandLabel(keyingMaterial, 'iv', new Uint8Array(0), IV_LEN),
      seq: 0n
    };
  }

  async function encryptRecord(keys, header, plain) {
    const key = await crypto.subtle.importKey('raw', keys.key, 'AES-GCM', false, ['encrypt']);
    const nonce = sequenceNonce(keys.iv, keys.seq++);
    return new Uint8Array(await crypto.subtle.encrypt({
      name: 'AES-GCM',
      iv: nonce,
      additionalData: header,
      tagLength: 128
    }, key, plain));
  }

  async function decryptRecord(keys, header, payload) {
    const key = await crypto.subtle.importKey('raw', keys.key, 'AES-GCM', false, ['decrypt']);
    const nonce = sequenceNonce(keys.iv, keys.seq++);
    return new Uint8Array(await crypto.subtle.decrypt({
      name: 'AES-GCM',
      iv: nonce,
      additionalData: header,
      tagLength: 128
    }, key, payload));
  }

  function sequenceNonce(iv, seq) {
    const nonce = new Uint8Array(iv);
    let n = seq;
    for (let i = nonce.byteLength - 1; i >= nonce.byteLength - 8; i -= 1) {
      nonce[i] ^= Number(n & 0xffn);
      n >>= 8n;
    }
    return nonce;
  }

  async function hkdfExtract(salt, ikm) {
    return hmac(salt.byteLength ? salt : new Uint8Array(HASH_LEN), ikm);
  }

  async function hkdfExpandLabel(secret, label, context, length) {
    const fullLabel = bytesOf(`tls13 ${label}`);
    const info = concat(u16(length), vec8(fullLabel), vec8(context));
    return hkdfExpand(secret, info, length);
  }

  async function deriveSecret(secret, label, transcriptHash) {
    return hkdfExpandLabel(secret, label, transcriptHash, HASH_LEN);
  }

  async function hkdfExpand(prk, info, length) {
    let okm = new Uint8Array(0);
    let t = new Uint8Array(0);
    for (let counter = 1; okm.byteLength < length; counter += 1) {
      t = await hmac(prk, concat(t, info, bytes(counter)));
      okm = concat(okm, t);
    }
    return okm.slice(0, length);
  }

  async function hmac(keyBytes, data) {
    const key = await crypto.subtle.importKey(
      'raw',
      keyBytes,
      { name: 'HMAC', hash: 'SHA-256' },
      false,
      ['sign']
    );
    return new Uint8Array(await crypto.subtle.sign('HMAC', key, data));
  }

  async function sha256(data) {
    return new Uint8Array(await crypto.subtle.digest('SHA-256', data));
  }

  async function verifyServerCertificateChain(leafDer, caChainPem, serverName, now = Date.now()) {
    if (!serverName) {
      throw new Error('missing expected server name');
    }
    if (!leafDer) {
      throw new Error('server did not send a certificate');
    }
    const roots = pemBlocks(caChainPem, 'CERTIFICATE');
    if (roots.length === 0) {
      throw new Error('enrolled profile has no daemon CA certificate');
    }
    if (roots.length !== 1) {
      throw new Error(`enrolled profile must contain exactly one daemon CA certificate, got ${roots.length}`);
    }
    const leaf = parseCertificate(leafDer);
    assertCertificateValidity(leaf, now, 'server certificate');
    assertServerCertificate(leaf, serverName);
    if (leaf.signatureAlgorithmOid !== OID_ECDSA_SHA256) {
      throw new Error(`unsupported server certificate signature algorithm ${leaf.signatureAlgorithmOid}`);
    }
    const root = parseCertificate(roots[0]);
    if (!constantTimeEqual(leaf.issuerDer, root.subjectDer)) {
      throw new Error('server certificate is not signed by the enrolled daemon CA');
    }
    assertCertificateValidity(root, now, 'daemon CA certificate');
    assertCertificateAuthority(root);
    const publicKey = await importEcdsaPublicKey(root.spkiDer);
    const ok = await crypto.subtle.verify(
      { name: 'ECDSA', hash: 'SHA-256' },
      publicKey,
      ecdsaDerSignatureToRaw(leaf.signatureValue),
      leaf.tbsDer
    );
    if (!ok) {
      throw new Error('server certificate is not signed by the enrolled daemon CA');
    }
  }

  function assertCertificateValidity(cert, now, label) {
    if (now < cert.notBefore || now > cert.notAfter) {
      throw new Error(`${label} is outside its validity period`);
    }
  }

  function assertCertificateAuthority(cert) {
    const basicConstraints = cert.extensions.get(OID_BASIC_CONSTRAINTS);
    if (!basicConstraints) {
      throw new Error('daemon CA certificate has no basic constraints');
    }
    const r = new DerReader(basicConstraints).constructed(0x30);
    if (r.remaining() === 0) {
      throw new Error('daemon CA certificate is not a certificate authority');
    }
    const ca = r.element(0x01).body;
    if (ca.byteLength !== 1 || ca[0] === 0) {
      throw new Error('daemon CA certificate is not a certificate authority');
    }
    if (r.remaining() > 0) {
      r.element(0x02);
    }
    r.done();
  }

  function assertServerCertificate(cert, serverName) {
    const extendedKeyUsage = cert.extensions.get(OID_EXTENDED_KEY_USAGE);
    if (!extendedKeyUsage) {
      throw new Error('server certificate has no extended key usage');
    }
    const usages = new DerReader(extendedKeyUsage).constructed(0x30);
    let hasServerAuth = false;
    while (usages.remaining() > 0) {
      hasServerAuth = parseOid(usages.element(0x06).body) === OID_SERVER_AUTH || hasServerAuth;
    }
    usages.done();
    if (!hasServerAuth) {
      throw new Error('server certificate is not authorized for server authentication');
    }

    const subjectAltName = cert.extensions.get(OID_SUBJECT_ALT_NAME);
    if (!subjectAltName) {
      throw new Error('server certificate has no subject alternative name');
    }
    const names = new DerReader(subjectAltName).constructed(0x30);
    const expectedIp = parseIpv4Address(serverName);
    let matched = false;
    while (names.remaining() > 0) {
      const name = names.element();
      if (expectedIp) {
        matched = (name.tag === 0x87 && constantTimeEqual(name.body, expectedIp)) || matched;
      } else if (!isIpAddress(serverName)) {
        matched = (name.tag === 0x82 && textDecoder.decode(name.body).toLowerCase() === serverName.toLowerCase()) || matched;
      }
    }
    names.done();
    if (!matched) {
      throw new Error(`server certificate does not match ${serverName}`);
    }
  }

  function parseIpv4Address(host) {
    const parts = host.split('.');
    if (parts.length !== 4 || parts.some((part) => !/^\d+$/.test(part))) {
      return null;
    }
    const octets = parts.map(Number);
    if (octets.some((octet) => octet < 0 || octet > 255)) {
      return null;
    }
    return new Uint8Array(octets);
  }

  async function importEcdsaPublicKey(spkiDer) {
    assertP256Spki(spkiDer);
    return crypto.subtle.importKey(
      'spki',
      spkiDer,
      { name: 'ECDSA', namedCurve: 'P-256' },
      false,
      ['verify']
    );
  }

  function parseCertificate(derBytes) {
    const certOuter = new DerReader(derBytes).constructed(0x30);
    const tbsDer = certOuter.elementBytes(0x30);
    const signatureAlgorithmOid = parseAlgorithmIdentifier(certOuter.elementBytes(0x30));
    const signatureValue = parseBitString(certOuter.elementBytes(0x03));
    certOuter.done();

    const tbs = new DerReader(tbsDer).constructed(0x30);
    if (tbs.peekTag() === 0xa0) {
      tbs.skipElement();
    }
    tbs.skipElement();
    tbs.skipElement();
    const issuerDer = tbs.elementBytes(0x30);
    const validity = parseCertificateValidity(tbs.elementBytes(0x30));
    const subjectDer = tbs.elementBytes(0x30);
    const spkiDer = tbs.elementBytes(0x30);
    let extensions = new Map();
    while (tbs.remaining() > 0) {
      const tag = tbs.peekTag();
      if (tag === 0x81 || tag === 0x82) {
        tbs.skipElement();
      } else if (tag === 0xa3) {
        extensions = parseCertificateExtensions(tbs.element(0xa3).body);
      } else {
        throw new Error(`unsupported certificate field 0x${tag.toString(16)}`);
      }
    }
    return {
      tbsDer,
      signatureAlgorithmOid,
      signatureValue,
      issuerDer,
      subjectDer,
      spkiDer,
      extensions,
      ...validity
    };
  }

  function parseCertificateExtensions(derBytes) {
    const r = new DerReader(derBytes).constructed(0x30);
    const extensions = new Map();
    while (r.remaining() > 0) {
      const extension = r.constructed(0x30);
      const oid = parseOid(extension.element(0x06).body);
      if (extension.remaining() > 0 && extension.peekTag() === 0x01) {
        const critical = extension.element(0x01).body;
        if (critical.byteLength !== 1) {
          throw new Error('invalid certificate extension critical flag');
        }
      }
      const value = extension.element(0x04).body;
      extension.done();
      if (extensions.has(oid)) {
        throw new Error(`duplicate certificate extension ${oid}`);
      }
      extensions.set(oid, value);
    }
    r.done();
    return extensions;
  }

  function parseCertificateValidity(derBytes) {
    const r = new DerReader(derBytes).constructed(0x30);
    const notBefore = parseAsn1Time(r.element());
    const notAfter = parseAsn1Time(r.element());
    r.done();
    return { notBefore, notAfter };
  }

  function parseAsn1Time(element) {
    const text = textDecoder.decode(element.body);
    const utc = element.tag === 0x17 && text.match(/^(\d{2})(\d{2})(\d{2})(\d{2})(\d{2})(\d{2})Z$/);
    const generalized = element.tag === 0x18 && text.match(/^(\d{4})(\d{2})(\d{2})(\d{2})(\d{2})(\d{2})Z$/);
    const parts = utc || generalized;
    if (!parts) {
      throw new Error('unsupported certificate time');
    }
    const shortYear = Number(parts[1]);
    const year = utc ? (shortYear >= 50 ? 1900 + shortYear : 2000 + shortYear) : shortYear;
    const month = Number(parts[2]);
    const day = Number(parts[3]);
    const hour = Number(parts[4]);
    const minute = Number(parts[5]);
    const second = Number(parts[6]);
    const value = Date.UTC(year, month - 1, day, hour, minute, second);
    const date = new Date(value);
    if (
      date.getUTCFullYear() !== year || date.getUTCMonth() !== month - 1 ||
      date.getUTCDate() !== day || date.getUTCHours() !== hour ||
      date.getUTCMinutes() !== minute || date.getUTCSeconds() !== second
    ) {
      throw new Error('invalid certificate time');
    }
    return value;
  }

  function assertP256Spki(spkiDer) {
    const spki = new DerReader(spkiDer).constructed(0x30);
    const algorithm = spki.constructed(0x30);
    const keyAlg = parseOid(algorithm.element(0x06).body);
    const curve = parseOid(algorithm.element(0x06).body);
    algorithm.done();
    const publicPoint = parseBitString(spki.elementBytes(0x03));
    spki.done();
    if (keyAlg !== OID_EC_PUBLIC_KEY || curve !== OID_PRIME256V1) {
      throw new Error('server certificate key is not ECDSA P-256');
    }
    if (publicPoint.byteLength !== 65 || publicPoint[0] !== 0x04) {
      throw new Error('server certificate public key is not an uncompressed P-256 point');
    }
  }

  function parseAlgorithmIdentifier(derBytes) {
    const r = new DerReader(derBytes).constructed(0x30);
    const oid = parseOid(r.element(0x06).body);
    while (r.remaining() > 0) {
      r.skipElement();
    }
    return oid;
  }

  function parseBitString(derBytes) {
    const el = new DerReader(derBytes).element(0x03);
    if (el.body.byteLength === 0 || el.body[0] !== 0) {
      throw new Error('unsupported DER BIT STRING');
    }
    return el.body.slice(1);
  }

  function parseOid(bytes) {
    if (bytes.byteLength === 0) {
      throw new Error('empty DER OID');
    }
    const parts = [Math.floor(bytes[0] / 40), bytes[0] % 40];
    let value = 0;
    for (let i = 1; i < bytes.byteLength; i += 1) {
      value = (value << 7) | (bytes[i] & 0x7f);
      if ((bytes[i] & 0x80) === 0) {
        parts.push(value);
        value = 0;
      }
    }
    if (value !== 0) {
      throw new Error('truncated DER OID');
    }
    return parts.join('.');
  }

  function ecdsaDerSignatureToRaw(derBytes, width = 32) {
    const sig = new DerReader(derBytes).constructed(0x30);
    const r = normalizeEcdsaInteger(sig.element(0x02).body, width);
    const s = normalizeEcdsaInteger(sig.element(0x02).body, width);
    sig.done();
    return concat(r, s);
  }

  function normalizeEcdsaInteger(bytes, width) {
    const value = stripLeadingZeroes(bytes);
    if (value.byteLength > width) {
      throw new Error('ECDSA signature integer is too wide');
    }
    const out = new Uint8Array(width);
    out.set(value, width - value.byteLength);
    return out;
  }

  function repeatedByte(value, len) {
    const out = new Uint8Array(len);
    out.fill(value);
    return out;
  }

  class DerReader {
    constructor(bytes) {
      this.bytes_ = toUint8(bytes);
      this.offset = 0;
    }
    remaining() {
      return this.bytes_.byteLength - this.offset;
    }
    peekTag() {
      this.require(1);
      return this.bytes_[this.offset];
    }
    constructed(tag) {
      return new DerReader(this.element(tag).body);
    }
    elementBytes(tag) {
      return this.element(tag).bytes;
    }
    element(expectedTag = null) {
      this.require(2);
      const start = this.offset;
      const tag = this.bytes_[this.offset++];
      if (expectedTag !== null && tag !== expectedTag) {
        throw new Error(`unexpected DER tag 0x${tag.toString(16)}`);
      }
      const len = this.length();
      this.require(len);
      const bodyStart = this.offset;
      this.offset += len;
      return {
        tag,
        bytes: this.bytes_.slice(start, this.offset),
        body: this.bytes_.slice(bodyStart, this.offset)
      };
    }
    skipElement() {
      this.element();
    }
    length() {
      const first = this.bytes_[this.offset++];
      if ((first & 0x80) === 0) {
        return first;
      }
      const count = first & 0x7f;
      if (count === 0 || count > 3) {
        throw new Error('unsupported DER length');
      }
      this.require(count);
      let len = 0;
      for (let i = 0; i < count; i += 1) {
        len = (len << 8) | this.bytes_[this.offset++];
      }
      return len;
    }
    require(len) {
      if (this.offset + len > this.bytes_.byteLength) {
        throw new Error('truncated DER');
      }
    }
    done() {
      if (this.remaining() !== 0) {
        throw new Error('trailing DER bytes');
      }
    }
  }

  function readExtensions(data) {
    const r = new Reader(data);
    const out = new Map();
    while (r.remaining() > 0) {
      const kind = r.u16();
      out.set(kind, r.bytes(r.u16()));
    }
    return out;
  }

  class Reader {
    constructor(bytes) {
      this.bytes_ = bytes;
      this.offset = 0;
    }
    remaining() {
      return this.bytes_.byteLength - this.offset;
    }
    u8() {
      this.require(1);
      return this.bytes_[this.offset++];
    }
    u16() {
      this.require(2);
      const v = readU16(this.bytes_, this.offset);
      this.offset += 2;
      return v;
    }
    u24() {
      this.require(3);
      const v = readU24(this.bytes_, this.offset);
      this.offset += 3;
      return v;
    }
    bytes(len) {
      this.require(len);
      const out = this.bytes_.slice(this.offset, this.offset + len);
      this.offset += len;
      return out;
    }
    require(len) {
      if (this.offset + len > this.bytes_.byteLength) {
        throw new Error('truncated TLS message');
      }
    }
  }

  function handshakeMessage(kind, body) {
    return concat(bytes(kind), u24(body.byteLength), body);
  }

  function record(type, payload) {
    return concat(recordHeader(type, payload.byteLength), payload);
  }

  function recordHeader(type, len) {
    return concat(bytes(type, 0x03, 0x03), u16(len));
  }

  function extension(kind, body) {
    return concat(u16(kind), vec16(body));
  }

  function vec8(body) {
    return concat(bytes(body.byteLength), body);
  }

  function vec16(body) {
    return concat(u16(body.byteLength), body);
  }

  function vec24(body) {
    return concat(u24(body.byteLength), body);
  }

  function u16(v) {
    return bytes((v >>> 8) & 0xff, v & 0xff);
  }

  function u24(v) {
    return bytes((v >>> 16) & 0xff, (v >>> 8) & 0xff, v & 0xff);
  }

  function readU16(bytes, offset) {
    return (bytes[offset] << 8) | bytes[offset + 1];
  }

  function readU24(bytes, offset) {
    return (bytes[offset] << 16) | (bytes[offset + 1] << 8) | bytes[offset + 2];
  }

  function readU32(bytes, offset) {
    return (
      (bytes[offset] * 0x1000000) +
      ((bytes[offset + 1] << 16) | (bytes[offset + 2] << 8) | bytes[offset + 3])
    );
  }

  function bytes(...values) {
    return new Uint8Array(values);
  }

  function bytesOf(text) {
    return textEncoder.encode(text);
  }

  function b64(bytes) {
    let bin = '';
    for (let i = 0; i < bytes.byteLength; i += 1) {
      bin += String.fromCharCode(bytes[i]);
    }
    return btoa(bin);
  }

  function concat(...parts) {
    const arrays = parts.map(toUint8);
    const total = arrays.reduce((sum, part) => sum + part.byteLength, 0);
    const out = new Uint8Array(total);
    let offset = 0;
    for (const part of arrays) {
      out.set(part, offset);
      offset += part.byteLength;
    }
    return out;
  }

  function toUint8(value) {
    if (value instanceof Uint8Array) {
      return value;
    }
    if (value instanceof ArrayBuffer) {
      return new Uint8Array(value);
    }
    if (ArrayBuffer.isView(value)) {
      return new Uint8Array(value.buffer, value.byteOffset, value.byteLength);
    }
    throw new Error('expected bytes');
  }

  function findBytes(haystack, needle) {
    outer:
    for (let i = 0; i <= haystack.byteLength - needle.byteLength; i += 1) {
      for (let j = 0; j < needle.byteLength; j += 1) {
        if (haystack[i + j] !== needle[j]) {
          continue outer;
        }
      }
      return i;
    }
    return -1;
  }

  function pemBlocks(pem, label) {
    const re = new RegExp(`-----BEGIN ${label}-----([^-]*)-----END ${label}-----`, 'g');
    const out = [];
    let match;
    while ((match = re.exec(pem)) !== null) {
      const b64 = match[1].replace(/\s+/g, '');
      const bin = atob(b64);
      const bytes = new Uint8Array(bin.length);
      for (let i = 0; i < bin.length; i += 1) {
        bytes[i] = bin.charCodeAt(i);
      }
      out.push(bytes);
    }
    return out;
  }

  function ecdsaRawSignatureToDer(rawSignature) {
    if (rawSignature.length % 2 !== 0) {
      throw new Error('invalid ECDSA signature length');
    }
    const width = rawSignature.length / 2;
    return derSequence(
      derInteger(rawSignature.slice(0, width)),
      derInteger(rawSignature.slice(width))
    );
  }

  function derSequence(...parts) {
    return der(0x30, ...parts);
  }

  function derInteger(bytes) {
    let value = stripLeadingZeroes(bytes);
    if (value.length === 0) {
      value = new Uint8Array([0]);
    }
    if (value[0] & 0x80) {
      value = concat(new Uint8Array([0]), value);
    }
    return der(0x02, value);
  }

  function der(tag, ...parts) {
    const body = concat(...parts);
    return concat(new Uint8Array([tag]), derLength(body.length), body);
  }

  function derLength(length) {
    if (length < 0x80) {
      return new Uint8Array([length]);
    }
    const bytes = [];
    let value = length;
    while (value > 0) {
      bytes.unshift(value & 0xff);
      value >>= 8;
    }
    return new Uint8Array([0x80 | bytes.length, ...bytes]);
  }

  function stripLeadingZeroes(bytes) {
    let offset = 0;
    while (offset < bytes.length - 1 && bytes[offset] === 0) {
      offset += 1;
    }
    return bytes.slice(offset);
  }

  function isIpAddress(host) {
    return /^\d+\.\d+\.\d+\.\d+$/.test(host) || host.includes(':');
  }

  function constantTimeEqual(a, b) {
    if (a.byteLength !== b.byteLength) {
      return false;
    }
    let diff = 0;
    for (let i = 0; i < a.byteLength; i += 1) {
      diff |= a[i] ^ b[i];
    }
    return diff === 0;
  }

  self.ZeroClawEnrollmentTls = {
    BrowserTls13Client,
    JsonRpcClient,
    TlsWebSocket,
    connectRpc,
    fetchEnrollmentTrust,
    enroll,
    _internals: {
      parseServerHello,
      pemBlocks,
      hkdfExpandLabel,
      parseCertificate,
      verifyServerCertificateChain,
      assertCertificateAuthority,
      assertServerCertificate,
      parseCertificateValidity,
      ecdsaDerSignatureToRaw
    }
  };
})();
"#;

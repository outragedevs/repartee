#
# rpe2e.pl — RPE2E v1.0 end-to-end encryption for irssi
#
# Copyright (c) 2026 repartee authors. MIT licensed.
#
# Wire-compatible with the native repartee implementation and the weechat
# rpe2e.py script. See docs/plans/2026-04-10-e2e-encryption-architecture.md
# for the protocol specification.
#
# Dependencies:
#   libsodium bindings:   cpan Crypt::NaCl::Sodium
#   JSON:                 cpan JSON::PP (in core since Perl 5.14)
#   MIME::Base64:         in Perl core
#
# Crypto binding notes:
#
#   The script uses a flat accessor style against the libsodium wrapper —
#   e.g. `$sodium->sign_keypair()`, `$sodium->sign_detached($msg, $sk)`,
#   `$sodium->sign_verify_detached($sig, $msg, $pk)`, `$sodium->hmac_sha256`,
#   `$sodium->hash_sha256`, `$sodium->aead_xchacha20poly1305_ietf_encrypt/
#   decrypt`, and `$sodium->scalarmult_curve25519_base($sk)` /
#   `$sodium->scalarmult_curve25519($sk, $peer_pk)` for X25519.
#
#   This matches the binding style already established in G1 of this file
#   (hmac_sha256, hash_sha256, aead_xchacha20poly1305_ietf_*, sign_keypair).
#   Deployments using upstream ambs/Crypt-NaCl-Sodium may need a thin shim
#   exposing these flat method names in terms of the sub-accessor API
#   (`$sodium->sign`, `$sodium->scalarmult`, `$sodium->auth`). Raw bytes
#   are expected from every call; if the binding returns Data::BytesLocker
#   objects, callers should `get_raw_bytes()` in the shim.
#
# Install:
#   cp scripts/irssi/rpe2e.pl ~/.irssi/scripts/autorun/
#   /load rpe2e
#   /e2e fingerprint      # show your SAS
#   /e2e on               # enable on the current channel
#

use strict;
use warnings;
use Irssi;
use Crypt::NaCl::Sodium qw(:utils);
use MIME::Base64;
use JSON::PP;
use File::Spec;

our $VERSION = '0.1.0';
our %IRSSI = (
    authors     => 'repartee',
    contact     => 'https://repart.ee',
    name        => 'rpe2e',
    description => 'RPE2E v1.0 end-to-end encryption (wire-compatible with repartee/weechat)',
    license     => 'MIT',
    url         => 'https://repart.ee',
);

my $sodium = Crypt::NaCl::Sodium->sodium;

# ── Protocol constants ────────────────────────────────────────────────────────
my $PROTO               = 'RPE2E01';
my $WIRE_PREFIX         = '+RPE2E01';
my $CTCP_TAG            = 'RPEE2E';
my $MAX_CHUNKS          = 16;
my $MAX_PT_PER_CHUNK    = 180;
my $TS_TOLERANCE        = 300;
my $KEYREQ_MIN_INTERVAL = 30;
my $HKDF_SALT           = 'RPE2E01-WRAP';

# ── Keyring storage ───────────────────────────────────────────────────────────

my $rpe2e_dir = File::Spec->catdir(Irssi::get_irssi_dir(), 'rpe2e');
mkdir $rpe2e_dir unless -d $rpe2e_dir;
chmod 0700, $rpe2e_dir;
my $keyring_path = File::Spec->catfile($rpe2e_dir, 'keyring.json');
my %rate_limit_sent;

sub empty_keyring {
    return {
        identity => undef,
        peers    => {},
        outgoing => {},
        incoming => {},
        channels => {},
        autotrust => [],
        pending  => {},
    };
}

sub load_keyring {
    unless (-f $keyring_path) {
        return empty_keyring();
    }
    open my $fh, '<', $keyring_path or return empty_keyring();
    local $/;
    my $json = <$fh>;
    close $fh;
    my $kr;
    eval { $kr = decode_json($json); };
    if ($@ or not ref $kr eq 'HASH') {
        Irssi::print("[E2E] keyring corrupt, starting fresh: $@");
        return empty_keyring();
    }
    for my $k (qw(identity peers outgoing incoming channels autotrust pending)) {
        $kr->{$k} //= ($k eq 'autotrust' ? [] : {});
    }
    return $kr;
}

sub save_keyring {
    my $kr = shift;
    open my $fh, '>', $keyring_path or do {
        Irssi::print("[E2E] cannot write keyring: $!");
        return;
    };
    print $fh encode_json($kr);
    close $fh;
    chmod 0600, $keyring_path;
}

sub now_unix { return time(); }

# ── Crypto wrappers ───────────────────────────────────────────────────────────

sub b64e { return encode_base64($_[0], ''); }
sub b64d { return decode_base64($_[0]); }

# Normalize a sodium return value (BytesLocker object or raw scalar) into a
# plain Perl byte-string that we can slice, unpack, concatenate, etc.
sub _raw {
    my $v = shift;
    return $v unless ref $v;
    return $v->get_raw_bytes if $v->can('get_raw_bytes');
    return "$v";
}

# ── X25519 (Curve25519 Diffie-Hellman) helpers ────────────────────────────────
#
# Generate a fresh X25519 keypair. RFC 7748 clamping is applied to the
# secret before the base-point multiplication so interop holds even if the
# underlying binding does not clamp internally.
sub generate_x25519_keypair {
    my $sk = _raw($sodium->random_bytes(32));
    my @b = unpack('C*', $sk);
    $b[0]  &= 248;
    $b[31] &= 127;
    $b[31] |= 64;
    $sk = pack('C*', @b);
    my $pk = _raw($sodium->scalarmult_curve25519_base($sk));
    return ($sk, $pk);
}

sub x25519_ecdh {
    my ($sk, $peer_pk) = @_;
    return _raw($sodium->scalarmult_curve25519($sk, $peer_pk));
}

sub generate_identity_pair {
    my ($pk, $sk) = $sodium->sign_keypair();
    return {
        pk         => b64e($pk->get_raw_bytes),
        sk         => b64e($sk->get_raw_bytes),
        created_at => now_unix(),
    };
}

sub compute_fingerprint {
    my $pk_b64 = shift;
    my $pk_raw = b64d($pk_b64);
    my $h = $sodium->hash_sha256($HKDF_SALT . ':FP:' . $pk_raw);
    return unpack('H*', substr($h, 0, 16));
}

sub get_or_init_identity {
    my $kr = load_keyring();
    unless ($kr->{identity}) {
        $kr->{identity} = generate_identity_pair();
        $kr->{identity}{fp} = compute_fingerprint($kr->{identity}{pk});
        save_keyring($kr);
        Irssi::print("[E2E] generated new identity: fp=" . $kr->{identity}{fp});
    }
    return $kr;
}

sub hkdf_sha256 {
    my ($salt, $ikm, $info, $length) = @_;
    # HKDF extract: PRK = HMAC-SHA256(salt, IKM)
    my $prk = $sodium->hmac_sha256($ikm, $salt);
    # HKDF expand: iterate HMAC with counter
    my $out = '';
    my $prev = '';
    my $counter = 1;
    while (length($out) < $length) {
        $prev = $sodium->hmac_sha256($prev . $info . chr($counter), $prk);
        $out .= $prev;
        $counter++;
    }
    return substr($out, 0, $length);
}

sub xchacha20_encrypt {
    my ($key, $aad, $pt) = @_;
    my $nonce = $sodium->random_bytes(24);
    my $ct = $sodium->aead_xchacha20poly1305_ietf_encrypt($pt, $aad, $nonce, $key);
    return ($nonce, $ct);
}

sub xchacha20_decrypt {
    my ($key, $nonce, $aad, $ct) = @_;
    return eval {
        $sodium->aead_xchacha20poly1305_ietf_decrypt($ct, $aad, $nonce, $key);
    };
}

sub build_aad {
    my ($chan, $msgid, $ts, $part, $total) = @_;
    return "$PROTO:$chan:" . $msgid . ':' . pack('q>', $ts) . ':' . chr($part) . ':' . chr($total);
}

# ── Wire format ───────────────────────────────────────────────────────────────

sub encode_wire {
    my ($msgid, $ts, $part, $total, $nonce, $ct) = @_;
    return sprintf(
        '%s %s %d %d/%d %s:%s',
        $WIRE_PREFIX,
        unpack('H*', $msgid),
        $ts,
        $part,
        $total,
        b64e($nonce),
        b64e($ct),
    );
}

sub parse_wire {
    my $line = shift;
    return undef unless $line =~ m{^\Q$WIRE_PREFIX\E\s+(\S+)\s+(\d+)\s+(\d+)/(\d+)\s+(\S+):(\S+)\s*$};
    my ($msgid_hex, $ts, $part, $total, $nonce_b64, $ct_b64) = ($1, $2, $3, $4, $5, $6);
    return undef if length($msgid_hex) != 16;
    return undef if $total < 1 or $total > $MAX_CHUNKS or $part < 1 or $part > $total;
    my $nonce = eval { b64d($nonce_b64) };
    return undef if $@ or length($nonce) != 24;
    my $ct = eval { b64d($ct_b64) };
    return undef if $@;
    return {
        msgid => pack('H*', $msgid_hex),
        ts    => $ts + 0,
        part  => $part + 0,
        total => $total + 0,
        nonce => $nonce,
        ct    => $ct,
    };
}

# ── Chunking (stateless) ──────────────────────────────────────────────────────

sub split_plaintext {
    my $pt = shift;
    return [''] if not defined $pt or $pt eq '';
    my @chunks;
    my $bytes = $pt;
    while (length($bytes) > 0) {
        my $chunk = substr($bytes, 0, $MAX_PT_PER_CHUNK);
        # Walk back to UTF-8 boundary if we split inside a continuation byte.
        while (length($chunk) > 0) {
            my $last = ord(substr($chunk, -1));
            last if ($last & 0xC0) != 0x80;
            $chunk = substr($chunk, 0, length($chunk) - 1);
        }
        last if length($chunk) == 0;
        push @chunks, $chunk;
        $bytes = substr($bytes, length($chunk));
        die "[E2E] chunk overflow (>$MAX_CHUNKS)" if @chunks > $MAX_CHUNKS;
    }
    return \@chunks;
}

# ── Command dispatch ──────────────────────────────────────────────────────────

sub push_info {
    my ($witem, $text) = @_;
    if ($witem) {
        $witem->print("[E2E] $text", MSGLEVEL_CLIENTCRAP);
    } else {
        Irssi::print("[E2E] $text");
    }
}

sub cmd_e2e {
    my ($data, $server, $witem) = @_;
    my @args = split /\s+/, ($data // '');
    my $sub = shift @args // '';
    if ($sub eq 'on') {
        cmd_on($witem);
    } elsif ($sub eq 'off') {
        cmd_off($witem);
    } elsif ($sub eq 'mode') {
        cmd_mode($witem, @args);
    } elsif ($sub eq 'fingerprint') {
        cmd_fingerprint($witem);
    } elsif ($sub eq 'list') {
        cmd_list($witem);
    } elsif ($sub eq 'status') {
        cmd_status();
    } elsif ($sub eq 'accept') {
        cmd_accept($witem, @args);
    } elsif ($sub eq 'revoke') {
        cmd_revoke($witem, @args);
    } elsif ($sub eq 'forget') {
        cmd_forget($witem, @args);
    } elsif ($sub eq 'rotate') {
        cmd_rotate($witem);
    } elsif ($sub eq 'handshake') {
        cmd_handshake($server, $witem, @args);
    } else {
        Irssi::print('[E2E] usage: /e2e <on|off|mode|fingerprint|accept|revoke|forget|list|status|rotate|handshake>');
    }
}

sub cmd_handshake {
    my ($server, $witem, $nick) = @_;
    unless ($server) {
        push_info($witem, 'no server for handshake');
        return;
    }
    unless ($witem and $witem->{type} eq 'CHANNEL') {
        push_info($witem, 'not on a channel — /e2e handshake must run in a channel window');
        return;
    }
    unless (defined $nick and length $nick) {
        push_info($witem, 'usage: /e2e handshake <nick>');
        return;
    }
    my $chan = $witem->{name};
    my $kr = get_or_init_identity();
    my $cfg = $kr->{channels}{$chan};
    unless ($cfg and $cfg->{enabled}) {
        push_info($witem, "e2e not enabled on $chan — /e2e on first");
        return;
    }
    my $req_wire = build_keyreq($kr, $chan);
    unless (defined $req_wire) {
        push_info($witem, 'failed to build KEYREQ');
        return;
    }
    save_keyring($kr);
    $server->send_raw("NOTICE $nick :$req_wire");
    $rate_limit_sent{$nick} = now_unix();
    push_info($witem, "sent KEYREQ to $nick for $chan");
}

sub cmd_on {
    my $witem = shift;
    unless ($witem and $witem->{type} eq 'CHANNEL') {
        push_info(undef, 'not on a channel');
        return;
    }
    my $kr = load_keyring();
    $kr->{channels}{ $witem->{name} } = { enabled => 1, mode => 'normal' };
    save_keyring($kr);
    push_info($witem, 'enabled on ' . $witem->{name} . ' (mode=normal)');
}

sub cmd_off {
    my $witem = shift;
    return unless $witem and $witem->{type} eq 'CHANNEL';
    my $kr = load_keyring();
    delete $kr->{channels}{ $witem->{name} };
    save_keyring($kr);
    push_info($witem, 'disabled on ' . $witem->{name});
}

sub cmd_mode {
    my ($witem, $mode) = @_;
    return unless $witem and $witem->{type} eq 'CHANNEL';
    $mode //= 'normal';
    unless ($mode =~ /^(auto-accept|normal|quiet)$/) {
        push_info($witem, "invalid mode: $mode");
        return;
    }
    my $kr = load_keyring();
    $kr->{channels}{ $witem->{name} } ||= { enabled => 1 };
    $kr->{channels}{ $witem->{name} }{mode} = $mode;
    save_keyring($kr);
    push_info($witem, "mode=$mode on " . $witem->{name});
}

sub cmd_fingerprint {
    my $witem = shift;
    my $kr = get_or_init_identity();
    my $fp = $kr->{identity}{fp};
    push_info($witem, "my fingerprint: $fp");
}

sub cmd_list {
    my $witem = shift;
    my $kr = load_keyring();
    my @rows;
    for my $key (sort keys %{ $kr->{incoming} || {} }) {
        my $s = $kr->{incoming}{$key};
        push @rows, sprintf('%s  %s  status=%s', $key, substr($s->{fp} // '', 0, 16), $s->{status} // 'pending');
    }
    if (@rows) {
        push_info($witem, "peers:\n  " . join("\n  ", @rows));
    } else {
        push_info($witem, 'no peers in keyring');
    }
}

sub cmd_status {
    my $kr = load_keyring();
    my $n_peers = scalar keys %{ $kr->{incoming} || {} };
    my $n_chan  = scalar grep { $kr->{channels}{$_}{enabled} } keys %{ $kr->{channels} || {} };
    my $fp = $kr->{identity} ? $kr->{identity}{fp} : '(none)';
    Irssi::print("[E2E] identity=$fp peers=$n_peers enabled_channels=$n_chan");
}

sub cmd_accept {
    my ($witem, $nick) = @_;
    return unless $witem and $nick;
    my $chan = $witem->{name};
    my $kr = load_keyring();
    # Find incoming session by nick (search any handle containing this nick)
    my $found;
    for my $key (keys %{ $kr->{incoming} }) {
        if ($key =~ /^\Q$nick\E\!/ and $key =~ /\|\Q$chan\E$/) {
            $found = $key;
            last;
        }
    }
    unless ($found) {
        push_info($witem, "no pending session for $nick on $chan");
        return;
    }
    $kr->{incoming}{$found}{status} = 'trusted';
    save_keyring($kr);
    push_info($witem, "$nick trusted on $chan");
}

sub cmd_revoke {
    my ($witem, $nick) = @_;
    return unless $witem and $nick;
    my $chan = $witem->{name};
    my $kr = load_keyring();
    for my $key (keys %{ $kr->{incoming} }) {
        if ($key =~ /^\Q$nick\E/ and $key =~ /\|\Q$chan\E$/) {
            $kr->{incoming}{$key}{status} = 'revoked';
        }
    }
    # Mark our outgoing session for lazy rotation on next send.
    if ($kr->{outgoing}{$chan}) {
        $kr->{outgoing}{$chan}{pending_rotation} = 1;
    }
    save_keyring($kr);
    push_info($witem, "$nick revoked on $chan — key will rotate on next message");
}

sub cmd_forget {
    my ($witem, $nick) = @_;
    return unless $nick;
    my $kr = load_keyring();
    for my $key (keys %{ $kr->{incoming} }) {
        delete $kr->{incoming}{$key} if $key =~ /^\Q$nick\E/;
    }
    save_keyring($kr);
    push_info($witem, "forgot $nick from keyring");
}

sub cmd_rotate {
    my $witem = shift;
    return unless $witem and $witem->{type} eq 'CHANNEL';
    my $chan = $witem->{name};
    my $kr = load_keyring();
    if ($kr->{outgoing}{$chan}) {
        $kr->{outgoing}{$chan}{pending_rotation} = 1;
        save_keyring($kr);
        push_info($witem, "rotation scheduled for $chan");
    } else {
        push_info($witem, "no outgoing session for $chan yet");
    }
}

# ── Outgoing session key management ───────────────────────────────────────────

sub get_or_generate_outgoing_key {
    my ($kr, $chan) = @_;
    my $sess = $kr->{outgoing}{$chan};
    if ($sess and not $sess->{pending_rotation}) {
        return b64d($sess->{sk});
    }
    my $fresh = _raw($sodium->random_bytes(32));
    $kr->{outgoing}{$chan} = {
        sk         => b64e($fresh),
        created_at => now_unix(),
        pending_rotation => 0,
    };
    save_keyring($kr);
    return $fresh;
}

# ── Handshake: KEYREQ / KEYRSP ────────────────────────────────────────────────
#
# Wire-compatible with src/e2e/handshake.rs. Both messages live inside the
# CTCP framing \x01RPEE2E ...\x01 and are transported via NOTICE (never
# PRIVMSG). The Ed25519 signature binds the ephemeral X25519 pubkey so a
# MitM cannot swap it; the wrap key derivation matches the native client's
# derive_wrap_key (X25519 ECDH → HKDF-SHA256 salt=RPE2E01-WRAP info=
# "RPE2E01-WRAP:<channel>").

sub _sig_payload_keyreq {
    my ($channel, $pubkey, $eph_x25519, $nonce) = @_;
    return 'KEYREQ:' . $channel . ':' . $pubkey . ':' . $eph_x25519 . ':' . $nonce;
}

sub _sig_payload_keyrsp {
    my ($channel, $pubkey, $eph_pub, $wrap_nonce, $wrap_ct, $nonce) = @_;
    return 'KEYRSP:' . $channel . ':' . $pubkey . ':' . $eph_pub . ':'
         . $wrap_nonce . ':' . $wrap_ct . ':' . $nonce;
}

sub _decode_hex_len {
    my ($s, $expected) = @_;
    return undef unless defined $s;
    return undef unless $s =~ /^[0-9a-fA-F]+$/;
    return undef if length($s) != $expected * 2;
    return pack('H*', $s);
}

sub _parse_kv {
    my @fields = @_;
    my %kv;
    for my $f (@fields) {
        my ($k, $v) = split /=/, $f, 2;
        next unless defined $k and defined $v;
        $kv{$k} = $v;
    }
    return \%kv;
}

sub parse_keyreq {
    my $body = shift;
    return undef unless defined $body;
    my @parts = split /\s+/, $body;
    return undef if @parts < 2;
    return undef unless $parts[0] eq $CTCP_TAG and $parts[1] eq 'KEYREQ';
    my $kv = _parse_kv(@parts[2 .. $#parts]);
    return undef unless defined $kv->{v} and $kv->{v} eq '1';
    my $channel = $kv->{chan};
    return undef unless defined $channel and length $channel;
    my $pubkey     = _decode_hex_len($kv->{pub},   32);
    my $eph_x25519 = _decode_hex_len($kv->{eph},   32);
    my $nonce      = _decode_hex_len($kv->{nonce}, 16);
    my $sig        = _decode_hex_len($kv->{sig},   64);
    return undef unless defined $pubkey and defined $eph_x25519
                    and defined $nonce  and defined $sig;
    return {
        channel    => $channel,
        pubkey     => $pubkey,
        eph_x25519 => $eph_x25519,
        nonce      => $nonce,
        sig        => $sig,
    };
}

sub parse_keyrsp {
    my $body = shift;
    return undef unless defined $body;
    my @parts = split /\s+/, $body;
    return undef if @parts < 2;
    return undef unless $parts[0] eq $CTCP_TAG and $parts[1] eq 'KEYRSP';
    my $kv = _parse_kv(@parts[2 .. $#parts]);
    return undef unless defined $kv->{v} and $kv->{v} eq '1';
    my $channel = $kv->{chan};
    return undef unless defined $channel and length $channel;
    my $pubkey     = _decode_hex_len($kv->{pub},    32);
    my $eph_pub    = _decode_hex_len($kv->{eph},    32);
    my $wrap_nonce = _decode_hex_len($kv->{wnonce}, 24);
    my $nonce      = _decode_hex_len($kv->{nonce},  16);
    my $sig        = _decode_hex_len($kv->{sig},    64);
    return undef unless defined $pubkey and defined $eph_pub
                    and defined $wrap_nonce and defined $nonce
                    and defined $sig;
    my $wrap_ct = eval { b64d($kv->{wrap} // '') };
    return undef if $@ or not defined $wrap_ct or length($wrap_ct) == 0;
    return {
        channel    => $channel,
        pubkey     => $pubkey,
        eph_pub    => $eph_pub,
        wrap_nonce => $wrap_nonce,
        wrap_ct    => $wrap_ct,
        nonce      => $nonce,
        sig        => $sig,
    };
}

sub build_keyreq {
    my ($kr, $channel) = @_;
    my $id = $kr->{identity};
    return undef unless $id;
    my ($eph_sk, $eph_pk) = generate_x25519_keypair();
    my $req_nonce = _raw($sodium->random_bytes(16));
    my $pubkey    = b64d($id->{pk});
    my $priv      = b64d($id->{sk});
    my $sig_payload = _sig_payload_keyreq($channel, $pubkey, $eph_pk, $req_nonce);
    my $sig = _raw($sodium->sign_detached($sig_payload, $priv));

    # Persist pending handshake keyed by channel (one in-flight per channel);
    # the responder does not echo the KEYREQ nonce in KEYRSP so we look up
    # by channel on arrival. Matches native E2eManager::pending layout.
    $kr->{pending}{$channel} = {
        eph_sk     => b64e($eph_sk),
        nonce      => b64e($req_nonce),
        created_at => now_unix(),
    };

    my $body = sprintf(
        '%s KEYREQ v=1 chan=%s pub=%s eph=%s nonce=%s sig=%s',
        $CTCP_TAG, $channel,
        unpack('H*', $pubkey),
        unpack('H*', $eph_pk),
        unpack('H*', $req_nonce),
        unpack('H*', $sig),
    );
    return "\x01$body\x01";
}

sub handle_keyreq {
    my ($kr, $sender_handle, $req_body) = @_;
    my $req = parse_keyreq($req_body);
    return undef unless $req;

    # Verify signature over the canonical payload.
    my $sig_payload = _sig_payload_keyreq(
        $req->{channel}, $req->{pubkey}, $req->{eph_x25519}, $req->{nonce},
    );
    my $ok = eval { $sodium->sign_verify_detached($req->{sig}, $sig_payload, $req->{pubkey}) };
    return undef if $@ or not $ok;

    # Channel must be e2e-enabled locally.
    my $ch = $kr->{channels}{ $req->{channel} };
    return undef unless $ch and $ch->{enabled};

    # Policy: auto-accept mode always responds; normal/quiet only responds
    # when a trusted incoming session already exists for this peer on this
    # channel. Matches src/e2e/manager.rs handle_keyreq.
    my $mode = $ch->{mode} // 'normal';
    my $existing = $kr->{incoming}{"$sender_handle|$req->{channel}"};
    my $allow = 0;
    if ($mode eq 'auto-accept') {
        $allow = 1;
    } elsif ($existing and ($existing->{status} // '') eq 'trusted') {
        $allow = 1;
    }
    return undef unless $allow;

    # TOFU upsert peer (pending status — explicit /e2e accept still required
    # in normal mode for any NEW peer; this branch only runs when already
    # trusted or in auto-accept).
    my $fp_hex = compute_fingerprint(b64e($req->{pubkey}));
    my $now = now_unix();
    my $prev_peer = $kr->{peers}{$fp_hex};
    $kr->{peers}{$fp_hex} = {
        pubkey      => b64e($req->{pubkey}),
        last_handle => $sender_handle,
        first_seen  => ($prev_peer ? $prev_peer->{first_seen} : $now),
        last_seen   => $now,
        status      => 'trusted',
    };

    # Generate responder ephemeral + ECDH + HKDF wrap key.
    my ($eph_sk, $eph_pk) = generate_x25519_keypair();
    my $shared   = x25519_ecdh($eph_sk, $req->{eph_x25519});
    my $info     = $HKDF_SALT . ':' . $req->{channel};
    my $wrap_key = hkdf_sha256($HKDF_SALT, $shared, $info, 32);

    # Wrap our outgoing session key for this channel.
    my $our_sk = get_or_generate_outgoing_key($kr, $req->{channel});
    my ($wrap_nonce, $wrap_ct) = xchacha20_encrypt($wrap_key, $info, $our_sk);
    $wrap_nonce = _raw($wrap_nonce);
    $wrap_ct    = _raw($wrap_ct);

    # Sign response binding our long-term pubkey + both ephemerals + ct.
    my $rsp_nonce  = _raw($sodium->random_bytes(16));
    my $our_pubkey = b64d($kr->{identity}{pk});
    my $our_priv   = b64d($kr->{identity}{sk});
    my $sig_payload2 = _sig_payload_keyrsp(
        $req->{channel}, $our_pubkey, $eph_pk,
        $wrap_nonce, $wrap_ct, $rsp_nonce,
    );
    my $sig = _raw($sodium->sign_detached($sig_payload2, $our_priv));

    save_keyring($kr);

    my $body = sprintf(
        '%s KEYRSP v=1 chan=%s pub=%s eph=%s wnonce=%s wrap=%s nonce=%s sig=%s',
        $CTCP_TAG, $req->{channel},
        unpack('H*', $our_pubkey),
        unpack('H*', $eph_pk),
        unpack('H*', $wrap_nonce),
        b64e($wrap_ct),
        unpack('H*', $rsp_nonce),
        unpack('H*', $sig),
    );
    return "\x01$body\x01";
}

sub handle_keyrsp {
    my ($kr, $sender_handle, $rsp_body) = @_;
    my $rsp = parse_keyrsp($rsp_body);
    return 0 unless $rsp;

    # Verify signature against the pubkey carried in the message itself —
    # TOFU pins this pubkey atomically with the first valid handshake.
    my $sig_payload = _sig_payload_keyrsp(
        $rsp->{channel}, $rsp->{pubkey}, $rsp->{eph_pub},
        $rsp->{wrap_nonce}, $rsp->{wrap_ct}, $rsp->{nonce},
    );
    my $ok = eval { $sodium->sign_verify_detached($rsp->{sig}, $sig_payload, $rsp->{pubkey}) };
    return 0 if $@ or not $ok;

    # Recover pending ephemeral secret for this channel.
    my $pending = $kr->{pending}{ $rsp->{channel} };
    return 0 unless $pending and defined $pending->{eph_sk};
    my $eph_sk = b64d($pending->{eph_sk});
    delete $kr->{pending}{ $rsp->{channel} };

    # ECDH + HKDF → wrap key, then unwrap session key.
    my $shared   = x25519_ecdh($eph_sk, $rsp->{eph_pub});
    my $info     = $HKDF_SALT . ':' . $rsp->{channel};
    my $wrap_key = hkdf_sha256($HKDF_SALT, $shared, $info, 32);
    my $sk = xchacha20_decrypt($wrap_key, $rsp->{wrap_nonce}, $info, $rsp->{wrap_ct});
    $sk = _raw($sk) if defined $sk;
    return 0 unless defined $sk and length($sk) == 32;

    # TOFU upsert peer as trusted — initiator consented by sending KEYREQ.
    my $fp_hex = compute_fingerprint(b64e($rsp->{pubkey}));
    my $now = now_unix();
    my $prev_peer = $kr->{peers}{$fp_hex};
    $kr->{peers}{$fp_hex} = {
        pubkey      => b64e($rsp->{pubkey}),
        last_handle => $sender_handle,
        first_seen  => ($prev_peer ? $prev_peer->{first_seen} : $now),
        last_seen   => $now,
        status      => 'trusted',
    };

    # Install the trusted incoming session so the next encrypted PRIVMSG
    # from this peer on this channel decrypts without further prompting.
    $kr->{incoming}{"$sender_handle|$rsp->{channel}"} = {
        fp         => $fp_hex,
        sk         => b64e($sk),
        status     => 'trusted',
        created_at => $now,
    };
    save_keyring($kr);
    return 1;
}

# ── Message encrypt hook ──────────────────────────────────────────────────────

sub signal_send_text {
    my ($data, $server, $witem) = @_;
    return unless $witem and $witem->{type} eq 'CHANNEL';
    return if $data =~ m{^/};   # commands pass through
    my $chan = $witem->{name};
    my $kr = load_keyring();
    my $cfg = $kr->{channels}{$chan};
    return unless $cfg and $cfg->{enabled};

    my $sk = get_or_generate_outgoing_key($kr, $chan);
    my $chunks = split_plaintext($data);
    my $total = scalar @$chunks;
    my $msgid = $sodium->random_bytes(8);
    my $ts    = now_unix();
    my $idx = 0;
    for my $chunk (@$chunks) {
        $idx++;
        my $aad = build_aad($chan, $msgid, $ts, $idx, $total);
        my ($nonce, $ct) = xchacha20_encrypt($sk, $aad, $chunk);
        my $wire = encode_wire($msgid, $ts, $idx, $total, $nonce, $ct);
        $server->command("MSG $chan $wire");
    }
    Irssi::signal_stop();
}

# ── Message decrypt hook ──────────────────────────────────────────────────────

sub signal_message_public {
    my ($server, $msg, $nick, $host, $target) = @_;
    my $wire = parse_wire($msg);
    return unless $wire;
    my $handle = "$nick!$host";
    my $kr = load_keyring();
    # Replay window check
    my $skew = abs(now_unix() - $wire->{ts});
    if ($skew > $TS_TOLERANCE) {
        Irssi::signal_stop();
        return;
    }
    my $sess = $kr->{incoming}{ "$handle|$target" };
    unless ($sess and ($sess->{status} // '') eq 'trusted') {
        # No trusted session — fire a KEYREQ if the rate limiter allows it.
        # 30-second per-peer window matches native E2eManager::allow_keyreq.
        my $last = $rate_limit_sent{$handle} // 0;
        if (now_unix() - $last >= $KEYREQ_MIN_INTERVAL) {
            $rate_limit_sent{$handle} = now_unix();
            my $kr2 = get_or_init_identity();
            my $cfg = $kr2->{channels}{$target};
            if ($cfg and $cfg->{enabled}) {
                my $req_wire = build_keyreq($kr2, $target);
                if (defined $req_wire and $server) {
                    $server->send_raw("NOTICE $nick :$req_wire");
                    save_keyring($kr2);
                }
            }
        }
        # Unknown peer — drop ciphertext, leave a hint in the buffer
        Irssi::signal_continue($server, "[E2E] unknown sender $handle on $target — no session (sent KEYREQ)", $nick, $host, $target);
        return;
    }
    my $sk_raw = b64d($sess->{sk});
    my $aad = build_aad($target, $wire->{msgid}, $wire->{ts}, $wire->{part}, $wire->{total});
    my $pt = xchacha20_decrypt($sk_raw, $wire->{nonce}, $aad, $wire->{ct});
    if (defined $pt) {
        Irssi::signal_continue($server, $pt, $nick, $host, $target);
    } else {
        Irssi::signal_stop();
    }
}

# ── CTCP NOTICE dispatch (handshake transport) ───────────────────────────────
#
# KEYREQ / KEYRSP travel as CTCP payloads inside NOTICE, never PRIVMSG. We
# hook 'message irc notice' at signal_add_first priority so we can consume
# the event (Irssi::signal_stop) and prevent the raw CTCP body from being
# rendered to the user's status window.
sub signal_irc_notice {
    my ($server, $msg, $nick, $host, $target) = @_;
    return unless defined $msg;
    return unless $msg =~ /^\x01(.*)\x01\s*$/;
    my $body = $1;
    return unless $body =~ /^\Q$CTCP_TAG\E\s/;

    my $kr = get_or_init_identity();
    my $sender_handle = "$nick!" . ($host // '');

    if ($body =~ /^\Q$CTCP_TAG\E\s+KEYREQ\s/) {
        my $rsp_wire = handle_keyreq($kr, $sender_handle, $body);
        if (defined $rsp_wire and $server) {
            $server->send_raw("NOTICE $nick :$rsp_wire");
        }
        Irssi::signal_stop();
        return;
    }
    if ($body =~ /^\Q$CTCP_TAG\E\s+KEYRSP\s/) {
        my $installed = handle_keyrsp($kr, $sender_handle, $body);
        if ($installed) {
            Irssi::print("[E2E] trusted session installed from $nick");
        }
        Irssi::signal_stop();
        return;
    }
    # Unknown RPEE2E subtype — swallow so it never leaks to the UI.
    Irssi::signal_stop();
}

# ── Init ──────────────────────────────────────────────────────────────────────

get_or_init_identity();

Irssi::command_bind('e2e', \&cmd_e2e);
Irssi::signal_add_first('send text', \&signal_send_text);
Irssi::signal_add_first('message public', \&signal_message_public);
Irssi::signal_add_first('message irc notice', \&signal_irc_notice);

Irssi::print("RPE2E $VERSION loaded — /e2e fingerprint to see your SAS");

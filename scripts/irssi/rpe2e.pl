#
# rpe2e.pl — RPE2E v1.0 end-to-end encryption for irssi
#
# Copyright (c) 2026 repartee authors. MIT licensed.
#
# Wire-compatible with the native repartee implementation and the weechat
# rpe2e.py script.
#

use strict;
use warnings;
use Irssi;
use Crypt::NaCl::Sodium qw(:utils);
use MIME::Base64 qw(encode_base64 decode_base64);
use JSON::PP qw(decode_json encode_json);
use File::Spec;
use Encode qw(encode decode FB_DEFAULT);
use Digest::SHA qw(hmac_sha256);
use DynaLoader ();
use FFI::Platypus 2.00;
use FFI::Platypus::Buffer qw(scalar_to_buffer grow);

our $VERSION = '0.2.0';
our %IRSSI = (
    authors     => 'repartee',
    contact     => 'https://repart.ee',
    name        => 'rpe2e',
    description => 'RPE2E v1.0 end-to-end encryption (wire-compatible with repartee/weechat)',
    license     => 'MIT',
    url         => 'https://repart.ee',
);

my $sodium = Crypt::NaCl::Sodium->new();
my $s_sign = $sodium->sign;
my $s_hash = $sodium->hash;
my $s_auth = $sodium->auth;
my $s_aead = $sodium->aead;
my $s_scalarmult = $sodium->scalarmult;
my $ffi = FFI::Platypus->new(api => 2);
my $libsodium = DynaLoader::dl_findfile('-lsodium') || DynaLoader::dl_findfile('-llibsodium');
die "libsodium not found" unless $libsodium;
$ffi->lib($libsodium);
$ffi->attach([sodium_init => '_ffi_sodium_init'] => [] => 'int');
$ffi->attach(
    [crypto_aead_xchacha20poly1305_ietf_encrypt => '_ffi_xchacha_encrypt'] => [
        'opaque', 'opaque', 'string', 'uint64', 'string', 'uint64', 'opaque', 'string', 'string'
    ] => 'int'
);
$ffi->attach(
    [crypto_aead_xchacha20poly1305_ietf_decrypt => '_ffi_xchacha_decrypt'] => [
        'opaque', 'opaque', 'opaque', 'string', 'uint64', 'string', 'uint64', 'string', 'string'
    ] => 'int'
);
$ffi->attach(
    [crypto_sign_ed25519_pk_to_curve25519 => '_ffi_pk_to_curve25519'] => ['opaque', 'string'] => 'int'
);
$ffi->attach(
    [crypto_sign_ed25519_sk_to_curve25519 => '_ffi_sk_to_curve25519'] => ['opaque', 'string'] => 'int'
);
_ffi_sodium_init();

my $PROTO                    = 'RPE2E01';
my $WIRE_PREFIX              = '+RPE2E01';
my $CTCP_TAG                 = 'RPEE2E';
my $MAX_CHUNKS               = 16;
my $MAX_PT_PER_CHUNK         = 180;
my $TS_TOLERANCE             = 300;
my $KEYREQ_MIN_INTERVAL      = 30;
my $PENDING_KEYREQ_TTL       = 120;
my $HKDF_SALT                = 'RPE2E01-WRAP';
my $CHANNEL_PREFIX_RE        = qr/^[#&!+]/;
my $DEBUG_ENABLED            = ($ENV{RPE2E_DEBUG} // '') eq '1';
my $DEBUG_BUFFER_ENABLED     = ($ENV{RPE2E_DEBUG_BUFFER} // '') eq '1';

my $rpe2e_dir = File::Spec->catdir(Irssi::get_irssi_dir(), 'rpe2e');
mkdir $rpe2e_dir unless -d $rpe2e_dir;
chmod 0700, $rpe2e_dir;
my $keyring_path = File::Spec->catfile($rpe2e_dir, 'keyring.json');
my $debug_log    = File::Spec->catfile($rpe2e_dir, 'rpe2e-debug.log');

my %rate_limit_sent;

sub _dbg {
    my ($msg) = @_;
    return unless $DEBUG_ENABLED;
    return unless open my $fh, '>>', $debug_log;
    print {$fh} scalar(localtime()) . " $msg\n";
    close $fh;
}

sub empty_keyring {
    return {
        identity             => undef,
        peers                => {},
        outgoing             => {},
        incoming             => {},
        channels             => {},
        pending              => {},
        autotrust            => [],
        outgoing_recipients  => {},
        pending_inbound      => {},
        pending_trust_change => [],
    };
}

# Load the keyring, distinguishing a genuine read/parse error from an absent
# file. Returns ($kr, $ok): $ok=0 means the file EXISTS but could not be read
# or parsed — the outbound gate must fail closed on that (an enabled config may
# be present but unreadable, so refusing beats risking plaintext). An absent
# file is ($empty, 1): there is legitimately no E2E state.
sub _load_keyring_checked {
    return (empty_keyring(), 1) unless -f $keyring_path;
    open my $fh, '<', $keyring_path or return (empty_keyring(), 0);
    local $/;
    my $json = <$fh>;
    close $fh;
    my $kr;
    eval { $kr = decode_json($json) };
    return (empty_keyring(), 0) if $@ || ref($kr) ne 'HASH';
    for my $k (qw(identity peers outgoing incoming channels pending outgoing_recipients pending_inbound)) {
        $kr->{$k} //= {};
    }
    $kr->{autotrust} //= [];
    $kr->{pending_trust_change} //= [];
    return ($kr, 1);
}

sub load_keyring {
    my ($kr, $ok) = _load_keyring_checked();
    Irssi::print("[E2E] keyring corrupt, starting fresh") if !$ok;
    return $kr;
}

sub save_keyring {
    my ($kr) = @_;
    my $tmp_path = $keyring_path . '.tmp.' . $$ . '.' . now_unix();
    open my $fh, '>', $tmp_path or do {
        Irssi::print("[E2E] cannot write keyring: $!");
        return;
    };
    print {$fh} encode_json($kr);
    close $fh;
    chmod 0600, $tmp_path;
    rename $tmp_path, $keyring_path or do {
        unlink $tmp_path;
        Irssi::print("[E2E] cannot replace keyring: $!");
        return;
    };
}

sub now_unix { return time(); }

sub _raw {
    my ($v) = @_;
    return $v unless ref $v;
    return $v->get_raw_bytes if $v->can('get_raw_bytes');
    return "$v";
}

sub b64e  { return encode_base64($_[0], ''); }
sub b64d  { return decode_base64($_[0]); }

sub b64u_encode {
    my ($bytes) = @_;
    my $out = encode_base64($bytes, '');
    $out =~ tr{+/}{-_};
    $out =~ s/=+\z//;
    return $out;
}

sub b64u_decode {
    my ($text) = @_;
    my $pad = (4 - (length($text) % 4)) % 4;
    my $copy = $text . ('=' x $pad);
    $copy =~ tr{-_}{+/};
    return decode_base64($copy);
}

sub fingerprint {
    my ($pk_raw) = @_;
    my $full = _raw($s_hash->sha256("RPE2E01-FP:" . $pk_raw));
    return substr($full, 0, 16);
}

sub fingerprint_hex {
    my ($fp_raw) = @_;
    return unpack('H*', $fp_raw);
}

sub ensure_identity {
    my $kr = load_keyring();
    if ($kr->{identity}) {
        return (b64d($kr->{identity}{pk}), b64d($kr->{identity}{sk}), pack('H*', $kr->{identity}{fp}));
    }
    my ($pk_obj, $sk_obj) = $s_sign->keypair();
    my $pk = _raw($pk_obj);
    my $sk = _raw($sk_obj);
    my $fp = fingerprint($pk);
    $kr->{identity} = {
        pk         => b64e($pk),
        sk         => b64e($sk),
        fp         => fingerprint_hex($fp),
        created_at => now_unix(),
    };
    save_keyring($kr);
    return ($pk, $sk, $fp);
}

sub ed25519_sign {
    my ($sk, $msg) = @_;
    return _raw($s_sign->mac($msg, $sk));
}

sub ed25519_verify {
    my ($pk, $msg, $sig) = @_;
    return eval { $s_sign->verify($sig, $msg, $pk) } ? 1 : 0;
}

sub generate_x25519_keypair {
    my $sk = _raw(random_bytes(32));
    my @b = unpack('C*', $sk);
    $b[0] &= 248;
    $b[31] &= 127;
    $b[31] |= 64;
    $sk = pack('C*', @b);
    my $pk = _raw($s_scalarmult->base($sk));
    return ($sk, $pk);
}

sub x25519_ecdh {
    my ($sk, $peer_pk) = @_;
    return _raw($s_scalarmult->shared_secret($sk, $peer_pk));
}

sub ed25519_pk_to_x25519 {
    my ($pk) = @_;
    my $out = '';
    grow $out, 32;
    my ($out_ptr, $out_size) = scalar_to_buffer($out);
    my $rc = _ffi_pk_to_curve25519($out_ptr, $pk);
    die "pk_to_curve25519 failed" if $rc != 0;
    return substr($out, 0, 32);
}

sub ed25519_sk_to_x25519_scalar {
    my ($sk, $pk) = @_;
    my $out = '';
    grow $out, 32;
    my ($out_ptr, $out_size) = scalar_to_buffer($out);
    my $rc = _ffi_sk_to_curve25519($out_ptr, $sk);
    die "sk_to_curve25519 failed" if $rc != 0;
    return substr($out, 0, 32);
}

sub hkdf_sha256 {
    my ($salt, $ikm, $info, $length) = @_;
    my $prk = hmac_sha256($ikm, $salt);
    my $out = '';
    my $prev = '';
    my $counter = 1;
    while (length($out) < $length) {
        $prev = hmac_sha256($prev . $info . chr($counter), $prk);
        $out .= $prev;
        $counter++;
    }
    return substr($out, 0, $length);
}

sub aead_encrypt {
    my ($key, $aad, $pt) = @_;
    my $nonce = _raw(random_bytes(24));
    my $ct = '';
    grow $ct, length($pt) + 16;
    my ($ct_ptr, $ct_size) = scalar_to_buffer($ct);
    my $ct_len = pack('Q<', 0);
    my ($ct_len_ptr, $ct_len_size) = scalar_to_buffer($ct_len);
    my $aad_in = defined($aad) ? $aad : '';
    my $rc = _ffi_xchacha_encrypt(
        $ct_ptr,
        $ct_len_ptr,
        $pt,
        length($pt),
        $aad_in,
        length($aad_in),
        undef,
        $nonce,
        $key,
    );
    die "xchacha encrypt failed" if $rc != 0;
    my $out_len = unpack('Q<', substr($ct_len, 0, 8));
    return ($nonce, substr($ct, 0, $out_len));
}

sub aead_decrypt {
    my ($key, $nonce, $aad, $ct) = @_;
    return eval {
        my $pt = '';
        my $max_len = length($ct) >= 16 ? length($ct) - 16 : 0;
        grow $pt, $max_len;
        my ($pt_ptr, $pt_size) = scalar_to_buffer($pt);
        my $pt_len = pack('Q<', 0);
        my ($pt_len_ptr, $pt_len_size) = scalar_to_buffer($pt_len);
        my $aad_in = defined($aad) ? $aad : '';
        my $rc = _ffi_xchacha_decrypt(
            $pt_ptr,
            $pt_len_ptr,
            undef,
            $ct,
            length($ct),
            $aad_in,
            length($aad_in),
            $nonce,
            $key,
        );
        die "xchacha decrypt failed" if $rc != 0;
        my $out_len = unpack('Q<', substr($pt_len, 0, 8));
        return substr($pt, 0, $out_len);
    };
}

sub build_aad {
    my ($channel, $msgid, $ts, $part, $total) = @_;
    my $chan = encode('UTF-8', $channel);
    return $PROTO
        . pack('n', length($chan)) . $chan
        . pack('n', 8) . $msgid
        . pack('n', 8) . pack('q>', $ts)
        . pack('n', 1) . chr($part)
        . pack('n', 1) . chr($total);
}

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
    my ($line) = @_;
    return undef unless defined $line;
    return undef unless $line =~ m{^\Q$WIRE_PREFIX\E\s+(\S+)\s+(\d+)\s+(\d+)/(\d+)\s+(\S+):(\S+)\s*$};
    my ($msgid_hex, $ts, $part, $total, $nonce_b64, $ct_b64) = ($1, $2, $3, $4, $5, $6);
    return undef if length($msgid_hex) != 16;
    return undef if $total < 1 || $total > $MAX_CHUNKS || $part < 1 || $part > $total;
    my $nonce = eval { b64d($nonce_b64) };
    return undef if $@ || !defined($nonce) || length($nonce) != 24;
    my $ct = eval { b64d($ct_b64) };
    return undef if $@ || !defined($ct);
    return {
        msgid => pack('H*', $msgid_hex),
        ts    => 0 + $ts,
        part  => 0 + $part,
        total => 0 + $total,
        nonce => $nonce,
        ct    => $ct,
    };
}

sub split_plaintext {
    my ($text) = @_;
    return split_plaintext_budget($text, $MAX_PT_PER_CHUNK);
}

# split_plaintext with a caller-chosen per-chunk byte budget — mirrors Rust
# src/e2e/chunker.rs::split_plaintext_budget. Used by the CTCP ACTION splitter,
# whose per-piece budget must leave room for the \x01ACTION …\x01 framing each
# piece is wrapped in afterwards. The MAX_CHUNKS cap applies regardless.
sub split_plaintext_budget {
    my ($text, $max_per_chunk) = @_;
    die "empty plaintext" unless defined $text && length $text;
    my $bytes = encode('UTF-8', $text);
    my @chunks;
    my $i = 0;
    while ($i < length($bytes)) {
        my $j = $i + $max_per_chunk;
        $j = length($bytes) if $j > length($bytes);
        while ($j > $i && $j < length($bytes) && ((ord(substr($bytes, $j, 1)) & 0xC0) == 0x80)) {
            $j--;
        }
        die "cannot split: UTF-8 codepoint too large" if $j == $i;
        push @chunks, substr($bytes, $i, $j - $i);
        die "chunk overflow" if @chunks > $MAX_CHUNKS;
        $i = $j;
    }
    return \@chunks;
}

sub _sig_payload_keyreq {
    my ($channel, $pub, $eph, $nonce) = @_;
    return 'KEYREQ:' . $channel . ':' . $pub . ':' . $eph . ':' . $nonce;
}

sub _sig_payload_keyrsp {
    my ($channel, $pub, $eph, $wn, $wrap, $nonce) = @_;
    return 'KEYRSP:' . $channel . ':' . $pub . ':' . $eph . ':' . $wn . ':' . $wrap . ':' . $nonce;
}

sub _sig_payload_keyrekey {
    my ($channel, $pub, $eph, $wn, $wrap, $nonce) = @_;
    return 'REKEY:' . $channel . ':' . $pub . ':' . $eph . ':' . $wn . ':' . $wrap . ':' . $nonce;
}

sub _parse_kv_strict {
    my (@fields) = @_;
    my %out;
    for my $field (@fields) {
        my ($k, $v) = split /=/, $field, 2;
        next unless defined $k && defined $v;
        return undef if exists $out{$k};
        $out{$k} = $v;
    }
    return \%out;
}

sub parse_keyreq {
    my ($body) = @_;
    my @parts = split /\s+/, ($body // '');
    return undef if @parts < 7;
    return undef unless $parts[0] eq $CTCP_TAG && $parts[1] eq 'KEYREQ';
    my $kv = _parse_kv_strict(@parts[2 .. $#parts]);
    return undef unless $kv && ($kv->{v} // '') eq '1';
    my ($channel, $pub, $eph, $nonce, $sig) = eval {
        (
            $kv->{c},
            b64u_decode($kv->{p}),
            b64u_decode($kv->{e}),
            b64u_decode($kv->{n}),
            b64u_decode($kv->{s}),
        )
    };
    return undef if $@;
    return undef unless defined $channel && length $channel;
    return undef unless length($pub) == 32 && length($eph) == 32 && length($nonce) == 16 && length($sig) == 64;
    return {
        channel    => $channel,
        pub        => $pub,
        eph_x25519 => $eph,
        nonce      => $nonce,
        sig        => $sig,
    };
}

sub parse_keyrsp {
    my ($body) = @_;
    my @parts = split /\s+/, ($body // '');
    return undef if @parts < 9;
    return undef unless $parts[0] eq $CTCP_TAG && $parts[1] eq 'KEYRSP';
    my $kv = _parse_kv_strict(@parts[2 .. $#parts]);
    return undef unless $kv && ($kv->{v} // '') eq '1';
    my ($channel, $pub, $eph, $wn, $wrap, $nonce, $sig) = eval {
        (
            $kv->{c},
            b64u_decode($kv->{p}),
            b64u_decode($kv->{e}),
            b64u_decode($kv->{wn}),
            b64u_decode($kv->{w}),
            b64u_decode($kv->{n}),
            b64u_decode($kv->{s}),
        )
    };
    return undef if $@;
    return undef unless defined $channel && length $channel;
    return undef unless length($pub) == 32 && length($eph) == 32 && length($wn) == 24 && length($nonce) == 16 && length($sig) == 64;
    return {
        channel    => $channel,
        pub        => $pub,
        eph_pub    => $eph,
        wrap_nonce => $wn,
        wrap_ct    => $wrap,
        nonce      => $nonce,
        sig        => $sig,
    };
}

sub parse_keyrekey {
    my ($body) = @_;
    my @parts = split /\s+/, ($body // '');
    return undef if @parts < 9;
    return undef unless $parts[0] eq $CTCP_TAG && $parts[1] eq 'REKEY';
    my $kv = _parse_kv_strict(@parts[2 .. $#parts]);
    return undef unless $kv && ($kv->{v} // '') eq '1';
    my ($channel, $pub, $eph, $wn, $wrap, $nonce, $sig) = eval {
        (
            $kv->{c},
            b64u_decode($kv->{p}),
            b64u_decode($kv->{e}),
            b64u_decode($kv->{wn}),
            b64u_decode($kv->{w}),
            b64u_decode($kv->{n}),
            b64u_decode($kv->{s}),
        )
    };
    return undef if $@;
    return undef unless defined $channel && length $channel;
    return undef unless length($pub) == 32 && length($eph) == 32 && length($wn) == 24 && length($nonce) == 16 && length($sig) == 64;
    return {
        channel    => $channel,
        pub        => $pub,
        eph_pub    => $eph,
        wrap_nonce => $wn,
        wrap_ct    => $wrap,
        nonce      => $nonce,
        sig        => $sig,
    };
}

sub _ctx_for_target {
    my ($target, $handle) = @_;
    return $target if defined $target && $target =~ $CHANNEL_PREFIX_RE;
    return '@' . $handle;
}

sub _find_peer_by_handle {
    my ($kr, $handle) = @_;
    for my $fp_hex (keys %{ $kr->{peers} }) {
        my $peer = $kr->{peers}{$fp_hex};
        return ($fp_hex, $peer) if ($peer->{last_handle} // '') eq $handle;
    }
    return;
}

sub _find_handle_by_nick {
    my ($kr, $nick) = @_;
    my $nick_lc = lc $nick;
    my ($best_seen, $best_handle);
    for my $fp_hex (keys %{ $kr->{peers} }) {
        my $peer = $kr->{peers}{$fp_hex};
        next unless defined $peer->{last_nick};
        next unless lc($peer->{last_nick}) eq $nick_lc;
        my $seen = $peer->{last_seen} // 0;
        if (!defined($best_seen) || $seen > $best_seen) {
            $best_seen = $seen;
            $best_handle = $peer->{last_handle};
        }
    }
    return $best_handle;
}

sub _classify_peer_change {
    my ($kr, $peer_fp_raw, $handle) = @_;
    my $fp_hex = fingerprint_hex($peer_fp_raw);
    if (my $peer = $kr->{peers}{$fp_hex}) {
        return 'revoked' if ($peer->{status} // '') eq 'revoked';
        return 'handle_changed:' . ($peer->{last_handle} // '') if ($peer->{last_handle} // '') ne $handle;
        return 'known';
    }
    my ($old_fp_hex) = _find_peer_by_handle($kr, $handle);
    return defined($old_fp_hex) ? 'fingerprint_changed:' . $old_fp_hex : 'new';
}

sub _glob_match_ci {
    my ($pattern, $text) = @_;
    my $re = quotemeta(lc $pattern);
    $re =~ s/\\\*/.*/g;
    $re =~ s/\\\?/./g;
    return lc($text) =~ /\A$re\z/ ? 1 : 0;
}

sub _autotrust_matches {
    my ($kr, $handle, $channel) = @_;
    for my $rule (@{ $kr->{autotrust} || [] }) {
        my $scope = $rule->{scope} // '';
        next unless $scope eq 'global' || $scope eq $channel;
        return 1 if _glob_match_ci($rule->{handle_pattern} // '', $handle);
    }
    return 0;
}

sub _record_pending_trust_change {
    my ($kr, $handle, $channel, $change, $new_pub, $old_fp, $new_fp) = @_;
    @{ $kr->{pending_trust_change} } = grep {
        !(($_->{handle} // '') eq $handle && ($_->{channel} // '') eq $channel)
    } @{ $kr->{pending_trust_change} || [] };
    push @{ $kr->{pending_trust_change} }, {
        handle      => $handle,
        channel     => $channel,
        change      => $change,
        new_pubkey  => defined($new_pub) ? b64e($new_pub) : undef,
        old_fp      => defined($old_fp) ? fingerprint_hex($old_fp) : undef,
        new_fp      => defined($new_fp) ? fingerprint_hex($new_fp) : undef,
        recorded_at => now_unix(),
    };
}

sub _take_pending_trust_changes {
    my ($kr, $handle) = @_;
    my @taken = grep { ($_->{handle} // '') eq $handle } @{ $kr->{pending_trust_change} || [] };
    @{ $kr->{pending_trust_change} } = grep { ($_->{handle} // '') ne $handle } @{ $kr->{pending_trust_change} || [] };
    return \@taken;
}

sub _allow_outgoing_keyreq {
    my ($handle) = @_;
    my $last = $rate_limit_sent{$handle} // 0;
    return 0 if now_unix() - $last < $KEYREQ_MIN_INTERVAL;
    $rate_limit_sent{$handle} = now_unix();
    return 1;
}

sub _get_or_generate_outgoing_key {
    my ($kr, $channel) = @_;
    my $row = $kr->{outgoing}{$channel};
    if ($row && !($row->{pending_rotation} // 0)) {
        return b64d($row->{sk});
    }
    my $fresh = _raw(random_bytes(32));
    $kr->{outgoing}{$channel} = {
        sk               => b64e($fresh),
        created_at       => now_unix(),
        pending_rotation => 0,
    };
    return $fresh;
}

sub _get_or_generate_outgoing_key_with_rotation {
    my ($kr, $channel) = @_;
    my $row = $kr->{outgoing}{$channel};
    if ($row && !($row->{pending_rotation} // 0)) {
        return (b64d($row->{sk}), 0);
    }
    my $had_pending = $row ? ($row->{pending_rotation} // 0) : 0;
    my $fresh = _raw(random_bytes(32));
    $kr->{outgoing}{$channel} = {
        sk               => b64e($fresh),
        created_at       => now_unix(),
        pending_rotation => 0,
    };
    return ($fresh, $had_pending ? 1 : 0);
}

sub _pending_key {
    my ($channel, $handle) = @_;
    return defined($handle) && length($handle) ? "$channel|$handle" : $channel;
}

sub build_keyreq {
    my ($kr, $channel, $handle) = @_;
    my ($pk, $sk, $fp_unused) = ensure_identity();
    my $pending_key = _pending_key($channel, $handle);
    if (my $pending = $kr->{pending}{$pending_key}) {
        if (now_unix() - ($pending->{created_at} // 0) < $PENDING_KEYREQ_TTL) {
            die "key exchange already pending for $pending_key";
        }
        delete $kr->{pending}{$pending_key};
    }
    my ($eph_sk, $eph_pk) = generate_x25519_keypair();
    my $nonce = _raw(random_bytes(16));
    my $sig = ed25519_sign($sk, _sig_payload_keyreq($channel, $pk, $eph_pk, $nonce));
    $kr->{pending}{$pending_key} = {
        eph_sk     => b64e($eph_sk),
        handle     => $handle,
        channel    => $channel,
        created_at => now_unix(),
    };
    return "\x01"
        . $CTCP_TAG
        . ' KEYREQ v=1'
        . ' c=' . $channel
        . ' p=' . b64u_encode($pk)
        . ' e=' . b64u_encode($eph_pk)
        . ' n=' . b64u_encode($nonce)
        . ' s=' . b64u_encode($sig)
        . "\x01";
}

sub _build_keyrsp_for_req {
    my ($kr, $channel, $sender_handle, $req_pub, $req_eph) = @_;
    my ($pk, $sk, $fp_unused) = ensure_identity();
    my ($eph_sk, $eph_pk) = generate_x25519_keypair();
    my $shared = x25519_ecdh($eph_sk, $req_eph);
    my $info = "RPE2E01-WRAP:$channel";
    my $wrap_key = hkdf_sha256($HKDF_SALT, $shared, $info, 32);
    my $our_sk = _get_or_generate_outgoing_key($kr, $channel);
    my ($wn, $wrap) = aead_encrypt($wrap_key, $info, $our_sk);
    my $nonce = _raw(random_bytes(16));
    my $sig = ed25519_sign($sk, _sig_payload_keyrsp($channel, $pk, $eph_pk, $wn, $wrap, $nonce));
    my $peer_fp = fingerprint($req_pub);
    my $fp_hex = fingerprint_hex($peer_fp);
    my $now = now_unix();
    my $peer = $kr->{peers}{$fp_hex} || {};
    $kr->{peers}{$fp_hex} = {
        %{$peer},
        pk          => b64e($req_pub),
        last_handle => $sender_handle,
        first_seen  => $peer->{first_seen} // $now,
        last_seen   => $now,
        status      => 'trusted',
    };
    $kr->{outgoing_recipients}{"$channel|$sender_handle"} = {
        channel       => $channel,
        handle        => $sender_handle,
        fingerprint   => $fp_hex,
        first_sent_at => $now,
    };
    return "\x01"
        . $CTCP_TAG
        . ' KEYRSP v=1'
        . ' c=' . $channel
        . ' p=' . b64u_encode($pk)
        . ' e=' . b64u_encode($eph_pk)
        . ' wn=' . b64u_encode($wn)
        . ' w=' . b64u_encode($wrap)
        . ' n=' . b64u_encode($nonce)
        . ' s=' . b64u_encode($sig)
        . "\x01";
}

sub _maybe_build_reciprocal_keyreq {
    my ($kr, $channel, $sender_handle) = @_;
    my $row = $kr->{incoming}{"$sender_handle|$channel"};
    my $pending = $kr->{pending}{ _pending_key($channel, $sender_handle) };
    my $already_trusted = $row && ($row->{status} // '') eq 'trusted';
    return undef if $pending || $already_trusted || !_allow_outgoing_keyreq($sender_handle);
    return build_keyreq($kr, $channel, $sender_handle);
}

sub _build_reciprocal_keyreq_on_accept {
    my ($kr, $channel, $sender_handle) = @_;
    my $row = $kr->{incoming}{"$sender_handle|$channel"};
    my $already_trusted = $row && ($row->{status} // '') eq 'trusted';
    return undef if $already_trusted;
    delete $kr->{pending}{ _pending_key($channel, $sender_handle) };
    return build_keyreq($kr, $channel, $sender_handle);
}

sub handle_keyreq {
    my ($kr, $sender_handle, $nick, $body) = @_;
    my $req = parse_keyreq($body);
    return (undef, undef, undef) unless $req;
    my $ctx = $req->{channel};
    _dbg("handle_keyreq sender=$nick!$sender_handle ctx=$ctx");
    return (undef, undef, $ctx) unless ed25519_verify($req->{pub}, _sig_payload_keyreq($ctx, $req->{pub}, $req->{eph_x25519}, $req->{nonce}), $req->{sig});
    my $cfg = $kr->{channels}{$ctx};
    return (undef, undef, $ctx) unless $cfg && ($cfg->{enabled} // 0);
    my $peer_fp = fingerprint($req->{pub});
    my $change = _classify_peer_change($kr, $peer_fp, $sender_handle);
    if ($change eq 'revoked') {
        _record_pending_trust_change($kr, $sender_handle, $ctx, 'revoked', undef, $peer_fp, $peer_fp);
        return (undef, undef, $ctx);
    }
    if ($change =~ /^handle_changed:/) {
        _record_pending_trust_change($kr, $sender_handle, $ctx, 'handle_changed', undef, $peer_fp, $peer_fp);
        return (undef, undef, $ctx);
    }
    if ($change =~ /^fingerprint_changed:(.+)$/) {
        my $old_fp = pack('H*', $1);
        _record_pending_trust_change($kr, $sender_handle, $ctx, 'fingerprint_changed', $req->{pub}, $old_fp, $peer_fp);
        return (undef, undef, $ctx);
    }
    my $fp_hex = fingerprint_hex($peer_fp);
    my $now = now_unix();
    my $peer = $kr->{peers}{$fp_hex} || {};
    $kr->{peers}{$fp_hex} = {
        %{$peer},
        pk          => b64e($req->{pub}),
        last_handle => $sender_handle,
        last_nick   => $nick,
        first_seen  => $peer->{first_seen} // $now,
        last_seen   => $now,
        status      => $peer->{status} // 'pending',
    };
    my $autotrust = _autotrust_matches($kr, $sender_handle, $ctx);
    my $mode = $cfg->{mode} // 'normal';
    $mode = 'auto-accept' if $autotrust;
    my $sess = $kr->{incoming}{"$sender_handle|$ctx"};
    my $already_trusted = $sess && ($sess->{status} // '') eq 'trusted';
    if ($mode eq 'quiet' && !$already_trusted) {
        return (undef, undef, $ctx);
    }
    if ($mode eq 'normal' && !$already_trusted && !$autotrust) {
        $kr->{incoming}{"$sender_handle|$ctx"} = {
            fp         => $fp_hex,
            sk         => b64e("\x00" x 32),
            status     => 'pending',
            created_at => $now,
        };
        $kr->{pending_inbound}{"$sender_handle|$ctx"} = {
            handle       => $sender_handle,
            channel      => $ctx,
            sender_handle=> $sender_handle,
            sender_nick  => $nick,
            pubkey       => b64e($req->{pub}),
            eph_x25519   => b64e($req->{eph_x25519}),
            nonce        => b64e($req->{nonce}),
            sig          => b64e($req->{sig}),
            received_at  => $now,
        };
        return (undef, undef, $ctx);
    }
    my $rsp = _build_keyrsp_for_req($kr, $ctx, $sender_handle, $req->{pub}, $req->{eph_x25519});
    my $reciprocal = _maybe_build_reciprocal_keyreq($kr, $ctx, $sender_handle);
    return ($rsp, $reciprocal, $ctx);
}

sub handle_keyrsp {
    my ($kr, $sender_handle, $nick, $body) = @_;
    my $rsp = parse_keyrsp($body);
    return (0, undef) unless $rsp;
    my $ctx = $rsp->{channel};
    return (0, $ctx) unless ed25519_verify($rsp->{pub}, _sig_payload_keyrsp($ctx, $rsp->{pub}, $rsp->{eph_pub}, $rsp->{wrap_nonce}, $rsp->{wrap_ct}, $rsp->{nonce}), $rsp->{sig});
    my $pending = delete $kr->{pending}{ _pending_key($ctx, $sender_handle) };
    if (!$pending) {
        $pending = delete $kr->{pending}{$ctx};
    }
    return (0, $ctx) unless $pending && defined $pending->{eph_sk};
    my $eph_sk = b64d($pending->{eph_sk});
    my $shared = x25519_ecdh($eph_sk, $rsp->{eph_pub});
    my $info = "RPE2E01-WRAP:$ctx";
    my $wrap_key = hkdf_sha256($HKDF_SALT, $shared, $info, 32);
    my $session_key = aead_decrypt($wrap_key, $rsp->{wrap_nonce}, $info, $rsp->{wrap_ct});
    return (0, $ctx) unless defined $session_key && length($session_key) == 32;
    my $peer_fp = fingerprint($rsp->{pub});
    my $change = _classify_peer_change($kr, $peer_fp, $sender_handle);
    if ($change eq 'revoked') {
        _record_pending_trust_change($kr, $sender_handle, $ctx, 'revoked', undef, $peer_fp, $peer_fp);
        return (0, $ctx);
    }
    if ($change =~ /^handle_changed:/) {
        _record_pending_trust_change($kr, $sender_handle, $ctx, 'handle_changed', undef, $peer_fp, $peer_fp);
        return (0, $ctx);
    }
    if ($change =~ /^fingerprint_changed:(.+)$/) {
        my $old_fp = pack('H*', $1);
        _record_pending_trust_change($kr, $sender_handle, $ctx, 'fingerprint_changed', $rsp->{pub}, $old_fp, $peer_fp);
        return (0, $ctx);
    }
    my $fp_hex = fingerprint_hex($peer_fp);
    my $now = now_unix();
    my $peer = $kr->{peers}{$fp_hex} || {};
    $kr->{peers}{$fp_hex} = {
        %{$peer},
        pk          => b64e($rsp->{pub}),
        last_handle => $sender_handle,
        last_nick   => $nick,
        first_seen  => $peer->{first_seen} // $now,
        last_seen   => $now,
        status      => 'trusted',
    };
    $kr->{incoming}{"$sender_handle|$ctx"} = {
        fp         => $fp_hex,
        sk         => b64e($session_key),
        status     => 'trusted',
        created_at => $now,
    };
    return (1, $ctx);
}

sub _build_rekey_for_peer {
    my ($kr, $channel, $peer_handle, $peer_pk, $new_sk) = @_;
    my ($pk, $sk, $fp_unused) = ensure_identity();
    my ($eph_sk, $eph_pk) = generate_x25519_keypair();
    my $peer_x25519 = ed25519_pk_to_x25519($peer_pk);
    my $shared = x25519_ecdh($eph_sk, $peer_x25519);
    my $info = "RPE2E01-REKEY:$channel";
    my $wrap_key = hkdf_sha256($HKDF_SALT, $shared, $info, 32);
    my ($wn, $wrap) = aead_encrypt($wrap_key, $info, $new_sk);
    my $nonce = _raw(random_bytes(16));
    my $sig = ed25519_sign($sk, _sig_payload_keyrekey($channel, $pk, $eph_pk, $wn, $wrap, $nonce));
    return "\x01"
        . $CTCP_TAG
        . ' REKEY v=1'
        . ' c=' . $channel
        . ' p=' . b64u_encode($pk)
        . ' e=' . b64u_encode($eph_pk)
        . ' wn=' . b64u_encode($wn)
        . ' w=' . b64u_encode($wrap)
        . ' n=' . b64u_encode($nonce)
        . ' s=' . b64u_encode($sig)
        . "\x01";
}

sub handle_rekey {
    my ($kr, $sender_handle, $nick, $body) = @_;
    my $rk = parse_keyrekey($body);
    return (0, undef) unless $rk;
    my $ctx = $rk->{channel};
    return (0, $ctx) unless ed25519_verify($rk->{pub}, _sig_payload_keyrekey($ctx, $rk->{pub}, $rk->{eph_pub}, $rk->{wrap_nonce}, $rk->{wrap_ct}, $rk->{nonce}), $rk->{sig});
    my $peer_fp = fingerprint($rk->{pub});
    my $change = _classify_peer_change($kr, $peer_fp, $sender_handle);
    return (0, $ctx) if $change eq 'new';
    if ($change eq 'revoked') {
        _record_pending_trust_change($kr, $sender_handle, $ctx, 'revoked', undef, $peer_fp, $peer_fp);
        return (0, $ctx);
    }
    if ($change =~ /^handle_changed:/) {
        _record_pending_trust_change($kr, $sender_handle, $ctx, 'handle_changed', undef, $peer_fp, $peer_fp);
        return (0, $ctx);
    }
    if ($change =~ /^fingerprint_changed:(.+)$/) {
        my $old_fp = pack('H*', $1);
        _record_pending_trust_change($kr, $sender_handle, $ctx, 'fingerprint_changed', $rk->{pub}, $old_fp, $peer_fp);
        return (0, $ctx);
    }
    my ($my_pk, $my_sk, $fp_unused) = ensure_identity();
    my $my_scalar = ed25519_sk_to_x25519_scalar($my_sk, $my_pk);
    my $shared = x25519_ecdh($my_scalar, $rk->{eph_pub});
    my $info = "RPE2E01-REKEY:$ctx";
    my $wrap_key = hkdf_sha256($HKDF_SALT, $shared, $info, 32);
    my $session_key = aead_decrypt($wrap_key, $rk->{wrap_nonce}, $info, $rk->{wrap_ct});
    return (0, $ctx) unless defined $session_key && length($session_key) == 32;
    my $fp_hex = fingerprint_hex($peer_fp);
    my $now = now_unix();
    my $peer = $kr->{peers}{$fp_hex} || {};
    $kr->{peers}{$fp_hex} = {
        %{$peer},
        pk          => b64e($rk->{pub}),
        last_handle => $sender_handle,
        last_nick   => $nick,
        first_seen  => $peer->{first_seen} // $now,
        last_seen   => $now,
        status      => 'trusted',
    };
    $kr->{incoming}{"$sender_handle|$ctx"} = {
        fp         => $fp_hex,
        sk         => b64e($session_key),
        status     => 'trusted',
        created_at => $now,
    };
    return (1, $ctx);
}

sub _distribute_rekey {
    my ($server, $kr, $channel, $new_sk) = @_;
    for my $key (keys %{ $kr->{outgoing_recipients} || {} }) {
        my $row = $kr->{outgoing_recipients}{$key};
        next unless ($row->{channel} // '') eq $channel;
        my $fp_hex = $row->{fingerprint};
        my $peer = $kr->{peers}{$fp_hex} or next;
        my $peer_pk = b64d($peer->{pk} // '');
        next unless length($peer_pk) == 32;
        my $nick = $peer->{last_nick};
        next unless defined $nick && length $nick;
        my $wire = eval { _build_rekey_for_peer($kr, $channel, $row->{handle}, $peer_pk, $new_sk) };
        if ($@ || !defined $wire) {
            my $witem = _notice_witem_for_ctx($server, $channel, $nick);
            _prnt_warn($witem, "rekey to $nick skipped: this Crypt::NaCl::Sodium binding cannot convert peer Ed25519 pubkeys for REKEY");
            next;
        }
        _send_raw_notice($server, $nick, $wire);
    }
}

sub _notice_witem_for_ctx {
    my ($server, $ctx, $nick) = @_;
    if (defined $ctx && $ctx =~ $CHANNEL_PREFIX_RE) {
        my $wi = $server->window_item_find($ctx);
        return $wi if $wi;
    }
    if (defined $nick && length $nick) {
        my $wi = $server->window_item_find($nick);
        return $wi if $wi;
    }
    return undef;
}

sub _prnt_ok {
    my ($witem, $msg) = @_;
    if ($witem) {
        $witem->print("[E2E] $msg", Irssi::MSGLEVEL_CLIENTCRAP());
    } else {
        Irssi::print("[E2E] $msg", Irssi::MSGLEVEL_CLIENTCRAP());
    }
}

sub _prnt_warn {
    my ($witem, $msg) = @_;
    if ($witem) {
        $witem->print("[E2E] $msg", Irssi::MSGLEVEL_CLIENTCRAP());
    } else {
        Irssi::print("[E2E] $msg", Irssi::MSGLEVEL_CLIENTCRAP());
    }
}

sub _prnt_err {
    my ($witem, $msg) = @_;
    if ($witem) {
        $witem->print("[E2E] $msg", Irssi::MSGLEVEL_CLIENTCRAP());
    } else {
        Irssi::print("[E2E] $msg", Irssi::MSGLEVEL_CLIENTCRAP());
    }
}

sub _prnt_dbg {
    my ($server, $ctx, $nick, $msg) = @_;
    return unless $DEBUG_BUFFER_ENABLED;
    my $witem = _notice_witem_for_ctx($server, $ctx, $nick);
    if ($witem) {
        $witem->print("[E2E debug] $msg", Irssi::MSGLEVEL_CLIENTCRAP());
    } else {
        Irssi::print("[E2E debug] $msg", Irssi::MSGLEVEL_CLIENTCRAP());
    }
}

sub _send_raw_notice {
    my ($server, $nick, $body) = @_;
    return unless $server;
    $server->send_raw_now("NOTICE $nick :$body");
}

sub _send_raw_privmsg {
    my ($server, $target, $body) = @_;
    return unless $server;
    $server->send_raw_now("PRIVMSG $target :$body");
}

sub _resolve_ctx_for_command {
    my ($kr, $witem, $nick) = @_;
    return undef unless $witem;
    my $target = $witem->{name};
    return $target if $target =~ $CHANNEL_PREFIX_RE;
    my $handle = _find_handle_by_nick($kr, $nick || $target);
    return defined $handle ? '@' . $handle : undef;
}

sub _resolve_handle_for_command {
    my ($kr, $nick_or_handle) = @_;
    return $nick_or_handle if defined($nick_or_handle) && $nick_or_handle =~ /@/;
    return _find_handle_by_nick($kr, $nick_or_handle);
}

sub cmd_on {
    my ($witem) = @_;
    unless ($witem && ($witem->{name} // '') =~ $CHANNEL_PREFIX_RE) {
        return _prnt_err(undef, 'not on a channel');
    }
    my $kr = load_keyring();
    $kr->{channels}{ $witem->{name} } = { enabled => 1, mode => 'normal' };
    save_keyring($kr);
    _prnt_ok($witem, "enabled on " . $witem->{name} . " (mode=normal)");
}

sub cmd_off {
    my ($witem) = @_;
    unless ($witem && ($witem->{name} // '') =~ $CHANNEL_PREFIX_RE) {
        return _prnt_err(undef, 'not on a channel');
    }
    my $kr = load_keyring();
    $kr->{channels}{ $witem->{name} } = { enabled => 0, mode => $kr->{channels}{ $witem->{name} }{mode} // 'normal' };
    save_keyring($kr);
    _prnt_ok($witem, "disabled on " . $witem->{name});
}

sub cmd_mode {
    my ($witem, $mode) = @_;
    unless ($witem && ($witem->{name} // '') =~ $CHANNEL_PREFIX_RE) {
        return _prnt_err(undef, 'not on a channel');
    }
    $mode //= 'normal';
    $mode = 'auto-accept' if $mode eq 'auto';
    return _prnt_err($witem, "invalid mode: $mode") unless $mode =~ /^(auto-accept|normal|quiet)$/;
    my $kr = load_keyring();
    $kr->{channels}{ $witem->{name} } = { enabled => 1, mode => $mode };
    save_keyring($kr);
    _prnt_ok($witem, "mode=$mode on " . $witem->{name});
}

sub cmd_fingerprint {
    my ($witem) = @_;
    my (undef, undef, $fp) = ensure_identity();
    _prnt_ok($witem, 'Fingerprint (mine):');
    _prnt_ok($witem, '  hex  ' . fingerprint_hex($fp));
}

sub cmd_status {
    my ($witem) = @_;
    my $kr = load_keyring();
    my $n_peers = scalar keys %{ $kr->{incoming} || {} };
    my $n_chan = scalar grep { ($kr->{channels}{$_}{enabled} // 0) } keys %{ $kr->{channels} || {} };
    my $fp = $kr->{identity} ? $kr->{identity}{fp} : '(none)';
    _prnt_ok($witem, "identity=$fp peers=$n_peers enabled_channels=$n_chan");
}

sub cmd_list {
    my ($witem, @args) = @_;
    my $all = grep { $_ eq '-all' } @args;
    my $kr = load_keyring();
    if ($all) {
        _prnt_ok($witem, 'Keyring (all)');
        my @peer_lines;
        for my $fp_hex (sort keys %{ $kr->{peers} || {} }) {
            my $p = $kr->{peers}{$fp_hex};
            push @peer_lines, sprintf(
                '  %s  [%s]  nick=%s fp=%s',
                ($p->{last_handle} // '—'),
                ($p->{status} // 'pending'),
                ($p->{last_nick} // '—'),
                substr($fp_hex, 0, 16),
            );
        }
        if (@peer_lines) {
            _prnt_ok($witem, 'Peers');
            _prnt_ok($witem, $_) for @peer_lines;
        }
        my @incoming_lines;
        for my $key (sort keys %{ $kr->{incoming} || {} }) {
            my ($handle, $channel) = split /\|/, $key, 2;
            my $row = $kr->{incoming}{$key};
            push @incoming_lines, sprintf(
                '  %s  %s  [%s]  fp=%s',
                $handle, $channel, ($row->{status} // 'pending'), substr(($row->{fp} // ''), 0, 16)
            );
        }
        if (@incoming_lines) {
            _prnt_ok($witem, 'Incoming Sessions');
            _prnt_ok($witem, $_) for @incoming_lines;
        }
        if (!@peer_lines && !@incoming_lines) {
            _prnt_ok($witem, '(no remembered E2E state)');
        }
        return;
    }
    unless ($witem) {
        return _prnt_err(undef, 'not in a chat buffer');
    }
    my $ctx = _resolve_ctx_for_command($kr, $witem, undef);
    return _prnt_err($witem, 'cannot resolve current context') unless defined $ctx;
    my @rows = grep { (split(/\|/, $_, 2))[1] eq $ctx } keys %{ $kr->{incoming} || {} };
    if (!@rows) {
        return _prnt_ok($witem, 'no peers');
    }
    for my $key (sort @rows) {
        my ($handle, $channel) = split /\|/, $key, 2;
        my $row = $kr->{incoming}{$key};
        _prnt_ok($witem, "  $handle on $channel  fp=" . substr(($row->{fp} // ''), 0, 16) . "  status=" . ($row->{status} // 'pending'));
    }
}

sub cmd_handshake {
    my ($server, $witem, $nick) = @_;
    return _prnt_err($witem, 'usage: /e2e handshake <nick>') unless defined $nick && length $nick;
    return _prnt_err($witem, 'not in a chat buffer') unless $witem;
    my $kr = load_keyring();
    my $ctx = _resolve_ctx_for_command($kr, $witem, $nick);
    return _prnt_err($witem, "cannot resolve handle for $nick") unless defined $ctx;
    my $cfg = $kr->{channels}{$ctx};
    return _prnt_err($witem, "e2e not enabled on $ctx") unless $cfg && ($cfg->{enabled} // 0);
    my $handle = _resolve_handle_for_command($kr, $nick);
    my $wire = eval { build_keyreq($kr, $ctx, $handle) };
    return _prnt_err($witem, "handshake failed: $@") if $@ || !defined $wire;
    save_keyring($kr);
    _send_raw_notice($server, $nick, $wire);
    _prnt_ok($witem, "KEYREQ sent to $nick for $ctx");
    _prnt_dbg($server, $ctx, $nick, "TX KEYREQ to $nick for $ctx");
}

sub cmd_accept {
    my ($server, $witem, $nick) = @_;
    return _prnt_err($witem, 'usage: /e2e accept <nick>') unless defined $nick && length $nick;
    my $kr = load_keyring();
    my $handle = _resolve_handle_for_command($kr, $nick);
    return _prnt_err($witem, "cannot resolve handle for $nick") unless defined $handle;
    my $ctx = _resolve_ctx_for_command($kr, $witem, $nick);
    return _prnt_err($witem, "cannot resolve context for $nick") unless defined $ctx;
    my $pending = delete $kr->{pending_inbound}{"$handle|$ctx"};
    if ($pending) {
        my $rsp = _build_keyrsp_for_req($kr, $ctx, $handle, b64d($pending->{pubkey}), b64d($pending->{eph_x25519}));
        my $reciprocal = eval { _build_reciprocal_keyreq_on_accept($kr, $ctx, $handle) };
        if ($@) {
            _dbg("cmd_accept reciprocal build failed for $nick ($handle) on $ctx: $@");
            $reciprocal = undef;
        }
        save_keyring($kr);
        _send_raw_notice($server, $nick, $rsp) if defined $rsp;
        _send_raw_notice($server, $nick, $reciprocal) if defined $reciprocal;
        _prnt_ok($witem, "accepted $nick ($handle) on $ctx — KEYRSP sent");
        _prnt_dbg($server, $ctx, $nick, "TX KEYRSP to $nick for $ctx");
        _prnt_dbg($server, $ctx, $nick, "TX KEYREQ to $nick for $ctx") if defined $reciprocal;
        return;
    }
    if (my $row = $kr->{incoming}{"$handle|$ctx"}) {
        $row->{status} = 'trusted';
        save_keyring($kr);
        return _prnt_ok($witem, "accepted $nick ($handle) on $ctx");
    }
    _prnt_err($witem, "no pending exchange or session for $nick on $ctx");
}

sub cmd_decline {
    my ($witem, $nick) = @_;
    return _prnt_err($witem, 'usage: /e2e decline <nick>') unless defined $nick && length $nick;
    my $kr = load_keyring();
    my $handle = _resolve_handle_for_command($kr, $nick);
    return _prnt_err($witem, "cannot resolve handle for $nick") unless defined $handle;
    my $ctx = _resolve_ctx_for_command($kr, $witem, $nick);
    return _prnt_err($witem, "cannot resolve context for $nick") unless defined $ctx;
    delete $kr->{pending_inbound}{"$handle|$ctx"};
    if (my $row = $kr->{incoming}{"$handle|$ctx"}) {
        $row->{status} = 'revoked';
    }
    save_keyring($kr);
    _prnt_warn($witem, "declined $nick on $ctx");
}

sub cmd_revoke {
    my ($witem, $nick) = @_;
    return _prnt_err($witem, 'usage: /e2e revoke <nick>') unless defined $nick && length $nick;
    my $kr = load_keyring();
    my $handle = _resolve_handle_for_command($kr, $nick);
    return _prnt_err($witem, "cannot resolve handle for $nick") unless defined $handle;
    my $ctx = _resolve_ctx_for_command($kr, $witem, $nick);
    return _prnt_err($witem, "cannot resolve context for $nick") unless defined $ctx;
    if (my $row = $kr->{incoming}{"$handle|$ctx"}) {
        $row->{status} = 'revoked';
    }
    delete $kr->{outgoing_recipients}{"$ctx|$handle"};
    if (my $out = $kr->{outgoing}{$ctx}) {
        $out->{pending_rotation} = 1;
    }
    my ($fp_hex, $peer) = _find_peer_by_handle($kr, $handle);
    $peer->{status} = 'revoked' if $peer;
    save_keyring($kr);
    _prnt_warn($witem, "revoked $nick on $ctx — key will rotate");
}

sub cmd_unrevoke {
    my ($witem, $nick) = @_;
    return _prnt_err($witem, 'usage: /e2e unrevoke <nick>') unless defined $nick && length $nick;
    my $kr = load_keyring();
    my $handle = _resolve_handle_for_command($kr, $nick);
    return _prnt_err($witem, "cannot resolve handle for $nick") unless defined $handle;
    my $ctx = _resolve_ctx_for_command($kr, $witem, $nick);
    return _prnt_err($witem, "cannot resolve context for $nick") unless defined $ctx;
    if (my $row = $kr->{incoming}{"$handle|$ctx"}) {
        $row->{status} = 'trusted';
    }
    my ($fp_hex, $peer) = _find_peer_by_handle($kr, $handle);
    $peer->{status} = 'trusted' if $peer;
    save_keyring($kr);
    _prnt_ok($witem, "unrevoked $nick on $ctx");
}

sub cmd_forget {
    my ($witem, @args) = @_;
    return _prnt_err($witem, 'usage: /e2e forget [-all] <nick|handle>') unless @args;
    my $all = @args > 1 && $args[0] eq '-all' ? shift(@args) : 0;
    if (@args > 1 && $args[-1] eq '-all') {
        pop @args;
        $all = 1;
    }
    my $kr = load_keyring();
    my $who = $args[0];
    my $handle = _resolve_handle_for_command($kr, $who);
    return _prnt_err($witem, "cannot resolve handle for $who") unless defined $handle;
    my $removed = 0;
    if ($all) {
        my ($fp_hex, $peer_unused) = _find_peer_by_handle($kr, $handle);
        $removed++ if defined $fp_hex && delete $kr->{peers}{$fp_hex};
        for my $store ($kr->{incoming}, $kr->{pending_inbound}, $kr->{outgoing_recipients}, $kr->{pending}) {
            for my $key (keys %{$store || {}}) {
                if ($key =~ /^\Q$handle\E\|/ || $key =~ /\|\Q$handle\E$/) {
                    delete $store->{$key};
                    $removed++;
                }
            }
        }
        @{ $kr->{pending_trust_change} } = grep { ($_->{handle} // '') ne $handle } @{ $kr->{pending_trust_change} || [] };
        save_keyring($kr);
        return _prnt_ok($witem, "forgot $who ($handle) globally — removed $removed row(s)");
    }
    my $ctx = _resolve_ctx_for_command($kr, $witem, $who);
    return _prnt_err($witem, "cannot resolve context for $who") unless defined $ctx;
    for my $store ($kr->{incoming}, $kr->{pending_inbound}) {
        my $key = "$handle|$ctx";
        if (delete $store->{$key}) {
            $removed++;
        }
    }
    save_keyring($kr);
    _prnt_ok($witem, "forgotten $who on $ctx");
}

sub cmd_verify {
    my ($witem, $nick) = @_;
    return _prnt_err($witem, 'usage: /e2e verify <nick>') unless defined $nick && length $nick;
    my $kr = load_keyring();
    my $handle = _resolve_handle_for_command($kr, $nick);
    return _prnt_err($witem, "cannot resolve handle for $nick") unless defined $handle;
    my $ctx = _resolve_ctx_for_command($kr, $witem, $nick);
    return _prnt_err($witem, "cannot resolve context for $nick") unless defined $ctx;
    my $row = $kr->{incoming}{"$handle|$ctx"};
    return _prnt_err($witem, "no session for $nick on $ctx") unless $row;
    my (undef, undef, $local_fp) = ensure_identity();
    _prnt_ok($witem, 'Fingerprint Verification');
    _prnt_ok($witem, '  You  ( local): ' . substr(fingerprint_hex($local_fp), 0, 16));
    _prnt_ok($witem, '  Them (' . $nick . '): ' . substr(($row->{fp} // ''), 0, 16));
}

sub cmd_reverify {
    my ($witem, $nick) = @_;
    return _prnt_err($witem, 'usage: /e2e reverify <nick>') unless defined $nick && length $nick;
    my $kr = load_keyring();
    my $handle = _resolve_handle_for_command($kr, $nick);
    return _prnt_err($witem, "cannot resolve handle for $nick") unless defined $handle;
    my $notices = _take_pending_trust_changes($kr, $handle);
    my $applied;
    for my $row (@$notices) {
        if (($row->{change} // '') eq 'fingerprint_changed' && defined $row->{new_pubkey} && defined $row->{new_fp}) {
            $applied = $row;
            last;
        }
    }
    if ($applied) {
        my $old_fp_hex = $applied->{old_fp};
        delete $kr->{peers}{$old_fp_hex} if defined $old_fp_hex;
        for my $store ($kr->{incoming}, $kr->{outgoing_recipients}) {
            for my $key (keys %{$store || {}}) {
                delete $store->{$key} if $key =~ /^\Q$handle\E\|/ || $key =~ /\|\Q$handle\E$/;
            }
        }
        $kr->{peers}{ $applied->{new_fp} } = {
            pk          => $applied->{new_pubkey},
            last_handle => $handle,
            last_nick   => $nick,
            first_seen  => now_unix(),
            last_seen   => now_unix(),
            status      => 'trusted',
        };
        save_keyring($kr);
        return _prnt_ok($witem, "reverified $nick: accepted new key fp=" . substr($applied->{new_fp}, 0, 16));
    }
    my ($old_fp_hex, $peer_unused) = _find_peer_by_handle($kr, $handle);
    delete $kr->{peers}{$old_fp_hex} if defined $old_fp_hex;
    for my $store ($kr->{incoming}, $kr->{outgoing_recipients}) {
        for my $key (keys %{$store || {}}) {
            delete $store->{$key} if $key =~ /^\Q$handle\E\|/ || $key =~ /\|\Q$handle\E$/;
        }
    }
    save_keyring($kr);
    _prnt_ok($witem, "reverified $nick: purged stale state; re-handshake to TOFU-pin the new key");
}

sub cmd_rotate {
    my ($witem) = @_;
    return _prnt_err($witem, 'not in a chat buffer') unless $witem;
    my $kr = load_keyring();
    my $ctx = _resolve_ctx_for_command($kr, $witem, undef);
    return _prnt_err($witem, 'cannot resolve current context') unless defined $ctx;
    if (my $out = $kr->{outgoing}{$ctx}) {
        $out->{pending_rotation} = 1;
    } else {
        $kr->{outgoing}{$ctx} = {
            sk               => b64e(_raw(random_bytes(32))),
            created_at       => now_unix(),
            pending_rotation => 1,
        };
    }
    save_keyring($kr);
    _prnt_ok($witem, "rotation scheduled for $ctx");
}

sub cmd_export {
    my ($witem, $path) = @_;
    return _prnt_err($witem, 'usage: /e2e export <file>') unless defined $path && length $path;
    my $kr = load_keyring();
    my $doc = {
        version    => 1,
        exportedAt => now_unix(),
        identity   => $kr->{identity},
        peers      => [
            map {
                my $fp = $_;
                +{ fingerprint => $fp, %{ $kr->{peers}{$fp} || {} } }
            } sort keys %{ $kr->{peers} || {} }
        ],
        incomingSessions => [
            map {
                my ($handle, $channel) = split /\|/, $_, 2;
                +{ handle => $handle, channel => $channel, %{ $kr->{incoming}{$_} || {} } }
            } sort keys %{ $kr->{incoming} || {} }
        ],
        outgoingSessions => [
            map {
                +{ channel => $_, %{ $kr->{outgoing}{$_} || {} } }
            } sort keys %{ $kr->{outgoing} || {} }
        ],
        channels => [
            map {
                +{ channel => $_, %{ $kr->{channels}{$_} || {} } }
            } sort keys %{ $kr->{channels} || {} }
        ],
        autotrust => $kr->{autotrust},
        outgoingRecipients => [
            values %{ $kr->{outgoing_recipients} || {} }
        ],
    };
    open my $fh, '>', $path or return _prnt_err($witem, "export failed: $!");
    print {$fh} JSON::PP->new->canonical->pretty->encode($doc);
    close $fh;
    chmod 0600, $path;
    _prnt_ok($witem, "exported keyring to $path");
}

sub cmd_import {
    my ($witem, $path) = @_;
    return _prnt_err($witem, 'usage: /e2e import <file>') unless defined $path && length $path;
    open my $fh, '<', $path or return _prnt_err($witem, "import failed: $!");
    local $/;
    my $json = <$fh>;
    close $fh;
    my $doc = eval { decode_json($json) };
    return _prnt_err($witem, "import failed: $@") if $@ || ref($doc) ne 'HASH';
    my $kr = empty_keyring();
    $kr->{identity} = $doc->{identity};
    for my $p (@{ $doc->{peers} || [] }) {
        my $fp = delete $p->{fingerprint};
        $kr->{peers}{$fp} = $p if defined $fp;
    }
    for my $s (@{ $doc->{incomingSessions} || [] }) {
        my $key = ($s->{handle} // '') . '|' . ($s->{channel} // '');
        $kr->{incoming}{$key} = {
            fp         => $s->{fp} // $s->{fingerprint},
            sk         => $s->{sk},
            status     => $s->{status},
            created_at => $s->{created_at} // $s->{createdAt},
        };
    }
    for my $o (@{ $doc->{outgoingSessions} || [] }) {
        my $channel = $o->{channel} // next;
        $kr->{outgoing}{$channel} = {
            sk               => $o->{sk},
            created_at       => $o->{created_at} // $o->{createdAt},
            pending_rotation => $o->{pending_rotation} // $o->{pendingRotation} // 0,
        };
    }
    for my $ch (@{ $doc->{channels} || [] }) {
        my $channel = $ch->{channel} // next;
        $kr->{channels}{$channel} = {
            enabled => $ch->{enabled} ? 1 : 0,
            mode    => $ch->{mode} // 'normal',
        };
    }
    $kr->{autotrust} = $doc->{autotrust} || [];
    for my $r (@{ $doc->{outgoingRecipients} || [] }) {
        my $key = ($r->{channel} // '') . '|' . ($r->{handle} // '');
        $kr->{outgoing_recipients}{$key} = $r;
    }
    save_keyring($kr);
    _prnt_ok($witem, "imported keyring from $path");
}

sub cmd_autotrust {
    my ($witem, @args) = @_;
    my $op = lc($args[0] // 'list');
    my $kr = load_keyring();
    if ($op eq 'list') {
        if (!@{ $kr->{autotrust} || [] }) {
            return _prnt_ok($witem, '(no autotrust rules)');
        }
        for my $rule (@{ $kr->{autotrust} }) {
            _prnt_ok($witem, '  ' . ($rule->{scope} // '') . '  ' . ($rule->{handle_pattern} // ''));
        }
        return;
    }
    if ($op eq 'add') {
        return _prnt_err($witem, 'usage: /e2e autotrust add <scope> <pattern>') unless @args >= 3;
        push @{ $kr->{autotrust} }, {
            scope          => $args[1],
            handle_pattern => $args[2],
            created_at     => now_unix(),
        };
        save_keyring($kr);
        return _prnt_ok($witem, "autotrust add $args[1] $args[2]");
    }
    if ($op eq 'remove') {
        return _prnt_err($witem, 'usage: /e2e autotrust remove <pattern>') unless @args >= 2;
        @{ $kr->{autotrust} } = grep { ($_->{handle_pattern} // '') ne $args[1] } @{ $kr->{autotrust} || [] };
        save_keyring($kr);
        return _prnt_ok($witem, "autotrust removed $args[1]");
    }
    _prnt_err($witem, 'usage: /e2e autotrust <list|add|remove>');
}

sub cmd_e2e {
    my ($data, $server, $witem) = @_;
    my @args = grep { length } split /\s+/, ($data // '');
    my $sub = lc(shift(@args) // '');
    if ($sub eq '' || $sub eq 'help') {
        _prnt_ok($witem, 'Encryption commands: on off mode fingerprint list status accept decline revoke unrevoke forget handshake verify reverify rotate export import autotrust');
    } elsif ($sub eq 'on') {
        cmd_on($witem);
    } elsif ($sub eq 'off') {
        cmd_off($witem);
    } elsif ($sub eq 'mode') {
        cmd_mode($witem, @args);
    } elsif ($sub eq 'fingerprint') {
        cmd_fingerprint($witem);
    } elsif ($sub eq 'list') {
        cmd_list($witem, @args);
    } elsif ($sub eq 'status') {
        cmd_status($witem);
    } elsif ($sub eq 'accept') {
        cmd_accept($server, $witem, @args);
    } elsif ($sub eq 'decline') {
        cmd_decline($witem, @args);
    } elsif ($sub eq 'revoke') {
        cmd_revoke($witem, @args);
    } elsif ($sub eq 'unrevoke') {
        cmd_unrevoke($witem, @args);
    } elsif ($sub eq 'forget') {
        cmd_forget($witem, @args);
    } elsif ($sub eq 'handshake') {
        cmd_handshake($server, $witem, @args);
    } elsif ($sub eq 'verify') {
        cmd_verify($witem, @args);
    } elsif ($sub eq 'reverify') {
        cmd_reverify($witem, @args);
    } elsif ($sub eq 'rotate') {
        cmd_rotate($witem);
    } elsif ($sub eq 'export') {
        cmd_export($witem, @args);
    } elsif ($sub eq 'import') {
        cmd_import($witem, @args);
    } elsif ($sub eq 'autotrust') {
        cmd_autotrust($witem, @args);
    } else {
        _prnt_err($witem, 'unknown subcommand');
    }
}

# ── Outbound E2E gate (F1) — mirror of e2e_encrypt_or_passthrough ─────────
#
# irssi's high-level `send text` signal only carries plain typed lines; `/me`
# (ACTION), `/msg`, `/say`, `/amsg`, … never pass through it, so they used to
# reach the wire as PLAINTEXT in an E2E conversation. The authoritative fix is
# a low-level chokepoint on `server sending command` (the same interception
# point the FiSH encryption script uses): every PRIVMSG about to hit the wire —
# regardless of the command that produced it — passes through the gate below.
# It NEVER downgrades an E2E-enabled conversation to plaintext; every refusal is
# visible.

# Themed refusal reasons (mirror Rust E2eRefusal::user_message).
my $REFUSE_NO_HANDLE = "cannot encrypt PM without peer handle — wait for a message from them first";
my $REFUSE_KEYRING   = "cannot encrypt — keyring read failed; message NOT sent (E2E stays on)";
my $REFUSE_ENCRYPT   = "encryption failed — message NOT sent as plaintext (use /e2e off to send cleartext)";
my $BOT_BYPASS_WARN  = "bot-style message to %s sent in CLEARTEXT — channel lines starting with '.' or '!' bypass encryption for bots";

# Encrypt plaintext under session key $sk for wire context $ctx; returns an
# arrayref of wire lines (one per chunk). Shared by the plain and ACTION gate
# paths so the wire framing has a single source of truth.
sub _encrypt_plain_wire {
    my ($sk, $ctx, $plain) = @_;
    my $chunks = split_plaintext($plain);
    my $total = scalar @$chunks;
    my $msgid = _raw(random_bytes(8));
    my $ts = now_unix();
    my @out;
    my $idx = 0;
    for my $chunk (@$chunks) {
        $idx++;
        my $aad = build_aad($ctx, $msgid, $ts, $idx, $total);
        my ($nonce, $ct) = aead_encrypt($sk, $aad, $chunk);
        push @out, encode_wire($msgid, $ts, $idx, $total, $nonce, $ct);
    }
    return \@out;
}

# Encrypt a \x01ACTION …\x01 CTCP frame — mirror of Rust
# E2eManager::encrypt_outgoing_ctcp. A frame fitting one chunk encrypts as-is;
# a longer ACTION splits into independent, individually-wrapped \x01ACTION
# piece\x01 frames (never fragmenting the CTCP envelope across bare chunks).
# Byte length, not character length, gates the one-chunk case.
sub _encrypt_ctcp_wire {
    my ($sk, $ctx, $frame) = @_;
    my $action_prefix = "\x01ACTION ";
    my $action_framing = length($action_prefix) + 1;   # +1 for trailing \x01
    if (length(encode('UTF-8', $frame)) <= $MAX_PT_PER_CHUNK) {
        return _encrypt_plain_wire($sk, $ctx, $frame);
    }
    die "CTCP frame exceeds one encrypted chunk and cannot be split"
        unless $frame =~ /^\Q$action_prefix\E/ && substr($frame, -1) eq "\x01";
    my $body = substr($frame, length($action_prefix), length($frame) - length($action_prefix) - 1);
    my $budget = $MAX_PT_PER_CHUNK - $action_framing;
    my $pieces = split_plaintext_budget($body, $budget);
    my @out;
    for my $piece (@$pieces) {
        my $piece_str = decode('UTF-8', $piece);
        push @out, @{ _encrypt_plain_wire($sk, $ctx, $action_prefix . $piece_str . "\x01") };
    }
    return \@out;
}

# Best-effort REKEY distribution that never dies. A distribution failure must
# NOT refuse the user's message: the key has already rotated (a revoked peer is
# excluded regardless) and a trusted peer that misses the REKEY re-handshakes on
# its next undecryptable ciphertext (auto-KEYREQ) — mirroring the Rust gate,
# whose REKEY NOTICEs are queued/retried rather than gating the send.
sub _distribute_rekey_safe {
    my ($server, $kr, $channel, $new_sk) = @_;
    eval { _distribute_rekey($server, $kr, $channel, $new_sk); 1 }
        or _dbg("_distribute_rekey_safe: distribution failed for $channel: $@");
}

# The gate decision for one outgoing PRIVMSG body. Returns ($action, $payload):
#   ('pass',   undef)      send $body unchanged (no E2E state / escape hatch)
#   ('cipher', [wire, …])  send these encrypted lines, drop the original
#   ('refuse', reason)     drop; caller shows the [E2E] reason
#   ('bypass', warn|undef) channel bot bypass; send plaintext, maybe warn
# Strip a leading STATUSMSG prefix (@#chan, +#chan, %#chan, …): a message to a
# channel subset is still that channel for E2E purposes. The lookahead keeps a
# real channel name whose first char is itself a channel prefix (#, &, !, +)
# untouched — only a non-channel char sitting in front of a channel prefix is
# stripped. Without this, `/msg @#secret …` classified as a DM to a bogus nick
# and leaked plaintext to the channel ops on an E2E-enabled channel.
sub _strip_statusmsg {
    my ($target) = @_;
    $target =~ s/^[\@%+&~]+(?=[#&!+])//;
    return $target;
}

# Resolve a DM peer's ident@host: LIVE first (an open query or a shared channel
# carries the peer's current ident@host, which a nick change does NOT alter),
# then the persisted cache by last_nick. Live resolution is what stops a renamed
# E2E peer from falling through to a plaintext send — the cache's last_nick can
# be stale (there is no 'event nick' handler), but the handle is unchanged and
# the live query/channel record still has it. Mirrors weechat's nicklist/query
# resolution.
sub _resolve_dm_handle {
    my ($server, $kr, $nick) = @_;
    if ($server) {
        my $q = eval { $server->query_find($nick) };
        return $q->{address} if $q && $q->{address};
        for my $ch (eval { $server->channels() }) {
            my $n = eval { $ch->nick_find($nick) };
            return $n->{host} if $n && $n->{host};
        }
    }
    return _find_handle_by_nick($kr, $nick);
}

sub _gate_decide {
    my ($server, $target, $body) = @_;
    # STATUSMSG targets (@#chan, +#chan) address a channel subset — classify and
    # key them by the underlying channel, never as a DM.
    my $chan = _strip_statusmsg($target);
    my $is_channel = $chan =~ $CHANNEL_PREFIX_RE;

    # Non-ACTION CTCP (VERSION, PING, …) is a deliberate plaintext escape hatch,
    # exactly as the Rust client leaves /ctcp and /version cleartext.
    my $is_ctcp   = length($body) >= 2 && substr($body, 0, 1) eq "\x01" && substr($body, -1) eq "\x01";
    my $is_action = $body =~ /^\x01ACTION / && substr($body, -1) eq "\x01";
    return ('pass', undef) if $is_ctcp && !$is_action;

    my ($kr, $ok) = _load_keyring_checked();

    # Bot-command bypass: `.cmd`/`!cmd` go out unencrypted so channel bots can
    # parse them. CHANNEL-ONLY and SINGLE-LINE; DMs never bypass. Warn whenever
    # we cannot POSITIVELY rule E2E out (enabled OR a keyring read error) so the
    # cleartext is never a silent surprise.
    if ($is_channel && $body =~ /^[.!]/ && index($body, "\n") < 0) {
        my $enabled = $kr->{channels}{$chan} && ($kr->{channels}{$chan}{enabled} // 0);
        my $warn = (!$ok || $enabled) ? sprintf($BOT_BYPASS_WARN, $chan) : undef;
        return ('bypass', $warn);
    }

    # A genuine keyring read error means an enabled config may exist but be
    # unreadable — refuse rather than risk plaintext (fail-closed).
    return ('refuse', $REFUSE_KEYRING) unless $ok;

    my $ctx;
    if ($is_channel) {
        $ctx = $chan;
    } else {
        my $handle = _resolve_dm_handle($server, $kr, $target);
        unless (defined $handle && length $handle) {
            # Nothing resolvable → no `@<handle>` config can exist for this nick
            # (a handshake would have cached one, and live resolution catches a
            # rename). Refuse only if an enabled legacy bare-nick row exists;
            # otherwise plaintext passthrough is safe (genuinely no E2E state).
            my $cfg = $kr->{channels}{$target};
            return ('refuse', $REFUSE_NO_HANDLE) if $cfg && ($cfg->{enabled} // 0);
            return ('pass', undef);
        }
        $ctx = '@' . $handle;
    }

    my $cfg = $kr->{channels}{$ctx};
    return ('pass', undef) unless $cfg && ($cfg->{enabled} // 0);

    # Persist only when a key is actually generated/rotated — otherwise the
    # existing key is returned unchanged and rewriting the whole keyring JSON on
    # every message is wasted I/O.
    my $pre_existing = $kr->{outgoing}{$ctx} && !($kr->{outgoing}{$ctx}{pending_rotation} // 0);
    my ($sk, $had_pending) = _get_or_generate_outgoing_key_with_rotation($kr, $ctx);
    _distribute_rekey_safe($server, $kr, $ctx, $sk) if $had_pending;
    save_keyring($kr) unless $pre_existing;
    my $wires = eval {
        $is_action ? _encrypt_ctcp_wire($sk, $ctx, $body)
                   : _encrypt_plain_wire($sk, $ctx, $body);
    };
    if ($@ || !defined $wires) {
        _dbg("_gate_decide encrypt failed for $ctx: $@");
        return ('refuse', $REFUSE_ENCRYPT);
    }
    return ('cipher', $wires);
}

# Fail-closed wrapper: any unexpected error in the decision resolves to a
# refusal, because at that point we can no longer rule out an enabled E2E
# context and must not leak plaintext.
sub _e2e_gate_wire {
    my ($server, $target, $body) = @_;
    my @res = eval { _gate_decide($server, $target, $body) };
    if ($@) {
        _dbg("_e2e_gate_wire OUTER EXCEPTION for $target: $@");
        return ('refuse', $REFUSE_KEYRING);
    }
    return @res;
}

# THE single authoritative outbound gate. Every PRIVMSG irssi is about to send —
# from plain typed text, `/me`, `/msg`, `/say`, `/amsg`, or a script — passes
# through here (there is no separate `send text` handler; irssi's own command
# layer produces the local echo, and this gate rewrites only the wire).
sub signal_server_sending_command {
    my ($server, $data) = @_;
    return unless $server && defined $data;
    my $line = $data;
    $line =~ s/\r*\n*\z//;   # some paths append CRLF; never let it into the body
    # Only PRIVMSG carries user message content. NOTICE (KEYREQ/KEYRSP/REKEY)
    # and every other command pass through untouched.
    return unless $line =~ /^PRIVMSG\s+(\S+)\s+:?(.*)$/s;
    my ($target, $body) = ($1, $2);
    # Re-entrancy guard: a STRUCTURALLY valid wire chunk is our own re-emit from
    # _send_raw_privmsg. Keyed on parse_wire (not a bare +RPE2E01 prefix) so a
    # user command like "/msg bob +RPE2E01 secret" still goes through the gate
    # instead of leaking as plaintext.
    return if parse_wire($body);
    my ($action, $payload) = _e2e_gate_wire($server, $target, $body);
    return if $action eq 'pass';
    if ($action eq 'bypass') {
        _prnt_warn(_notice_witem_for_ctx($server, $target, $target), $payload) if defined $payload;
        return;
    }
    if ($action eq 'refuse') {
        # irssi's command layer already echoed the plaintext locally, so make it
        # explicit that nothing was delivered (the wire line is dropped below).
        _prnt_err(_notice_witem_for_ctx($server, $target, $target),
                  "$payload — the message shown above was NOT delivered");
        Irssi::signal_stop();   # drop the plaintext line — never reaches the wire
        return;
    }
    # cipher: suppress the original and emit the encrypted chunks ourselves.
    Irssi::signal_stop();
    _send_raw_privmsg($server, $target, $_) for @$payload;
}

# Decrypt one RPE2E wire message. Returns ($handled, $decoded):
#   (0, undef)      — not an RPE2E wire; caller lets irssi handle it
#   (1, undef)      — handled by dropping (ts skew / no session→KEYREQ /
#                     decrypt fail); this sub already called signal_stop()
#   (1, $plaintext) — decrypted; caller re-drives the DECRYPTED text through
#                     the pipeline (so a \x01ACTION…\x01 frame renders as an
#                     action instead of raw control bytes)
# `$target` is the CONTEXT target: the channel for channel messages, the
# SENDER's nick for DMs (DM context = @<sender handle>).
sub _decrypt_wire_message {
    my ($server, $msg, $nick, $host, $target) = @_;
    my $wire = parse_wire($msg);
    return (0, undef) unless $wire;
    my $handle = $host;
    my $ctx = _ctx_for_target($target, $handle);
    my $kr = load_keyring();
    _dbg("wire from $nick!$handle -> $target part=$wire->{part}/$wire->{total}");
    if (abs(now_unix() - $wire->{ts}) > $TS_TOLERANCE) {
        _prnt_dbg($server, $ctx, $nick, "drop ciphertext from $nick ($handle): timestamp skew");
        Irssi::signal_stop();
        return (1, undef);
    }
    my $row = $kr->{incoming}{"$handle|$ctx"};
    if (!$row || ($row->{status} // '') ne 'trusted') {
        _prnt_dbg($server, $ctx, $nick, "no trusted incoming for ($handle,$ctx)");
        if (_allow_outgoing_keyreq($handle)) {
            my $cfg = $kr->{channels}{$ctx};
            if ($cfg && ($cfg->{enabled} // 0)) {
                my $req = eval { build_keyreq($kr, $ctx, $handle) };
                if (!$@ && defined $req) {
                    _send_raw_notice($server, $nick, $req);
                    save_keyring($kr);
                    my $witem = _notice_witem_for_ctx($server, $ctx, $nick);
                    _prnt_ok($witem, "KEYREQ sent to $nick for $ctx");
                    _prnt_dbg($server, $ctx, $nick, "TX auto-KEYREQ to $nick for $ctx");
                }
            }
        }
        Irssi::signal_stop();
        return (1, undef);
    }
    my $aad = build_aad($ctx, $wire->{msgid}, $wire->{ts}, $wire->{part}, $wire->{total});
    my $pt = aead_decrypt(b64d($row->{sk}), $wire->{nonce}, $aad, $wire->{ct});
    unless (defined $pt) {
        _prnt_dbg($server, $ctx, $nick, "decrypt failed for ($handle,$ctx)");
        Irssi::signal_stop();
        return (1, undef);
    }
    my $decoded = eval { decode('UTF-8', $pt, FB_DEFAULT) };
    $decoded = decode('UTF-8', $pt) if !defined $decoded;
    return (1, $decoded);
}

# F2: decrypt on `event privmsg` — the irssi analog of weechat's raw-line
# `irc_in2_privmsg` modifier, firing BEFORE irssi splits out CTCP/ACTION. An
# encrypted ACTION travels on the wire as `+RPE2E01…` (no \x01), so decrypting
# it here yields `\x01ACTION…\x01` which re-enters the pipeline and renders as a
# proper action; decrypting later (on `message public`/`private`, post-CTCP-
# split) rendered raw control characters instead.
sub signal_event_privmsg {
    my ($server, $data, $nick, $host) = @_;
    return unless defined $data;
    # `$data` is "target :text" (the PRIVMSG params). For a channel, target is
    # the channel; for a DM, target is OUR nick and the sender is $nick.
    my ($target, $text) = $data =~ /^(\S+)\s+:?(.*)$/s;
    return unless defined $target && defined $text;
    # STATUSMSG incoming (@#chan) still keys off the underlying channel.
    my $chan = _strip_statusmsg($target);
    my $is_channel = $chan =~ $CHANNEL_PREFIX_RE;
    # Context target: channel → the channel; DM → the SENDER's nick (DM context
    # is keyed off the sender's handle, matching the old message-private path).
    my $ctx_target = $is_channel ? $chan : $nick;
    # _decrypt_wire_message returns (0, undef) for a non-RPE2E line (no second
    # parse needed here), (1, undef) when it already dropped the ciphertext, or
    # (1, $plaintext) on success.
    my ($handled, $decoded) = _decrypt_wire_message($server, $text, $nick, $host, $ctx_target);
    return unless $handled && defined $decoded;
    # A decrypted non-ACTION CTCP frame (\x01VERSION\x01, \x01PING\x01, DCC, …)
    # must NOT be re-driven through the pipeline: irssi would interpret it and
    # auto-answer with a PLAINTEXT NOTICE. Our own outbound side never encrypts
    # non-ACTION CTCP (it is a cleartext escape hatch), so this is anomalous —
    # drop it visibly instead of executing it. ACTION frames and ordinary text
    # re-enter normally.
    if ($decoded =~ /^\x01/ && $decoded !~ /^\x01ACTION /) {
        _prnt_warn(_notice_witem_for_ctx($server, $ctx_target, $nick),
                   "dropped an encrypted non-ACTION CTCP from $nick");
        Irssi::signal_stop();
        return;
    }
    # Re-drive the DECRYPTED text through the same event, preserving the wire's
    # original target so routing (channel vs query) is unchanged. irssi then
    # performs the CTCP/ACTION split on cleartext.
    Irssi::signal_continue($server, "$target :$decoded", $nick, $host);
}

sub _handle_rpee2e_ctcp_reply {
    my ($server, $args, $nick, $host, $target) = @_;
    return unless defined $args;
    my $inner = $args =~ /^\Q$CTCP_TAG\E\s/ ? $args : ($CTCP_TAG . ' ' . $args);
    my $sender_handle = $host // '';
    my $kr = load_keyring();
    if ($inner =~ /^\Q$CTCP_TAG\E\s+KEYREQ\s/) {
        my $parsed = parse_keyreq($inner);
        my $ctx = $parsed ? $parsed->{channel} : '';
        _prnt_dbg($server, $ctx, $nick, "RX KEYREQ from $nick ($sender_handle) for $ctx") if $parsed;
        my ($rsp, $reciprocal, $ctx_unused) = handle_keyreq($kr, $sender_handle, $nick, $inner);
        save_keyring($kr);
        if ($parsed && !$rsp && exists $kr->{pending_inbound}{"$sender_handle|$ctx"}) {
            my $witem = _notice_witem_for_ctx($server, $ctx, $nick);
            _prnt_warn($witem, "Pending key exchange from $nick ($sender_handle) for $ctx. Run /e2e accept <nick> or /e2e decline <nick>.");
            _prnt_dbg($server, $ctx, $nick, "KEYREQ from $nick ($sender_handle) is pending on $ctx");
        }
        if ($rsp) {
            _send_raw_notice($server, $nick, $rsp);
            _prnt_dbg($server, $ctx, $nick, "TX KEYRSP to $nick for $ctx");
        }
        if ($reciprocal) {
            _send_raw_notice($server, $nick, $reciprocal);
            _prnt_dbg($server, $ctx, $nick, "TX reciprocal KEYREQ to $nick for $ctx");
        }
        Irssi::signal_stop();
        return;
    }
    if ($inner =~ /^\Q$CTCP_TAG\E\s+KEYRSP\s/) {
        my $parsed = parse_keyrsp($inner);
        my $ctx = $parsed ? $parsed->{channel} : '';
        _prnt_dbg($server, $ctx, $nick, "RX KEYRSP from $nick ($sender_handle) for $ctx") if $parsed;
        my ($ok, $ctx_unused) = handle_keyrsp($kr, $sender_handle, $nick, $inner);
        save_keyring($kr);
        _prnt_dbg($server, $ctx, $nick, "KEYRSP from $nick ($sender_handle) installed session on $ctx") if $ok && $parsed;
        Irssi::signal_stop();
        return;
    }
    if ($inner =~ /^\Q$CTCP_TAG\E\s+REKEY\s/) {
        my $parsed = parse_keyrekey($inner);
        my $ctx = $parsed ? $parsed->{channel} : '';
        my ($ok, $ctx_unused) = handle_rekey($kr, $sender_handle, $nick, $inner);
        save_keyring($kr);
        _prnt_dbg($server, $ctx, $nick, "REKEY from $nick ($sender_handle) installed on $ctx") if $ok && $parsed;
        Irssi::signal_stop();
        return;
    }
    Irssi::signal_stop();
}

sub signal_ctcp_reply_rpee2e {
    my ($server, $args, $nick, $host, $target) = @_;
    _handle_rpee2e_ctcp_reply($server, $args, $nick, $host, $target);
}

sub signal_ctcp_reply_generic {
    my ($server, $args, $nick, $host, $target) = @_;
    return unless defined $args;
    return unless $args =~ /^\Q$CTCP_TAG\E(?:\s|$)/;
    _handle_rpee2e_ctcp_reply($server, $args, $nick, $host, $target);
}

sub signal_default_ctcp_reply_generic {
    my ($server, $args, $nick, $host, $target) = @_;
    return unless defined $args;
    return unless $args =~ /^\Q$CTCP_TAG\E(?:\s|$)/;
    Irssi::signal_stop();
}

ensure_identity();

Irssi::command_bind('e2e', \&cmd_e2e);
# THE single authoritative outbound fail-closed gate (F1): every PRIVMSG on the
# wire — plain typed text, `/me`, `/msg`, `/say`, `/amsg`, scripts — passes
# through here. irssi's own command layer produces the local echo.
Irssi::signal_add_first('server sending command', \&signal_server_sending_command);
# F2: decrypt on the raw PRIVMSG event, BEFORE irssi's CTCP/ACTION split, so
# an encrypted ACTION renders as an action rather than raw control characters.
Irssi::signal_add_first('event privmsg', \&signal_event_privmsg);
Irssi::signal_add_first('ctcp reply RPEE2E', \&signal_ctcp_reply_rpee2e);
Irssi::signal_add_first('ctcp reply', \&signal_ctcp_reply_generic);
Irssi::signal_add_first('default ctcp reply', \&signal_default_ctcp_reply_generic);

Irssi::print("RPE2E $VERSION loaded — /e2e fingerprint to see your fingerprint");

1;

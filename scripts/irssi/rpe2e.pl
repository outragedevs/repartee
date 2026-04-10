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
    my ($sender, $chan, $msgid, $ts, $part, $total) = @_;
    return "$PROTO:$sender:$chan:" . $msgid . ':' . pack('q>', $ts) . ':' . chr($part) . ':' . chr($total);
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
    } else {
        Irssi::print('[E2E] usage: /e2e <on|off|mode|fingerprint|accept|revoke|forget|list|status|rotate>');
    }
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
    my $fresh = $sodium->random_bytes(32);
    $kr->{outgoing}{$chan} = {
        sk         => b64e($fresh),
        created_at => now_unix(),
        pending_rotation => 0,
    };
    save_keyring($kr);
    return $fresh;
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
    my $my_handle = (($server->{userhost} || $server->{nick}) =~ /@/)
        ? $server->{userhost}
        : $server->{nick} . '!' . ($server->{userhost} // 'unknown@unknown');
    # irssi doesn't always surface our userhost; fall back to nick!unknown
    if ($my_handle !~ /\@/) {
        $my_handle = $server->{nick} . '!unknown@unknown';
    }
    my $idx = 0;
    for my $chunk (@$chunks) {
        $idx++;
        my $aad = build_aad($my_handle, $chan, $msgid, $ts, $idx, $total);
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
    unless ($sess and $sess->{status} eq 'trusted') {
        # Unknown peer — drop ciphertext, leave a hint in the buffer
        Irssi::signal_continue($server, "[E2E] unknown sender $handle on $target — no session", $nick, $host, $target);
        return;
    }
    my $sk_raw = b64d($sess->{sk});
    my $aad = build_aad($handle, $target, $wire->{msgid}, $wire->{ts}, $wire->{part}, $wire->{total});
    my $pt = xchacha20_decrypt($sk_raw, $wire->{nonce}, $aad, $wire->{ct});
    if (defined $pt) {
        Irssi::signal_continue($server, $pt, $nick, $host, $target);
    } else {
        Irssi::signal_stop();
    }
}

# ── Init ──────────────────────────────────────────────────────────────────────

get_or_init_identity();

Irssi::command_bind('e2e', \&cmd_e2e);
Irssi::signal_add_first('send text', \&signal_send_text);
Irssi::signal_add_first('message public', \&signal_message_public);

Irssi::print("RPE2E $VERSION loaded — /e2e fingerprint to see your SAS");

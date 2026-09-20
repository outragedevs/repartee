import test from 'node:test';
import assert from 'node:assert/strict';
import {parseMessage, notification, navigation, fold} from '../web-ui/push/payload.js';
const config = {nick: 'Me', label: 'Network', chantypes: '#&', statusmsg: '@+', casemapping: 'rfc1459'};

test('Soju single IRC messages preserve text and escaped tags', () => {
    const msg = parseMessage('@msgid=a\\:b;test=x\\sy :Alice!u@h PRIVMSG Me :100% <b>hello</b>\r\n');
    assert.equal(msg.tags.msgid, 'a;b');
    assert.equal(msg.tags.test, 'x y');
    assert.equal(msg.params[1], '100% <b>hello</b>');
    assert.equal(parseMessage(':A PRIVMSG Me :one\r\n:A PRIVMSG Me :two'), null);
    assert.equal(parseMessage('{"notification":true}'), null);
});

test('private messages route to sender; channel status prefixes are removed', () => {
    assert.equal(notification(':Alice!u@h PRIVMSG mE :hello', config).target, 'Alice');
    assert.equal(notification(':Alice!u@h PRIVMSG @#chat :hello', config).target, '#chat');
    assert.equal(notification(':Alice!u@h PRIVMSG +local :hello', {...config,chantypes:'#&+'}).target, '+local');
    for (const target of ['++local','@+local','@++local']) assert.equal(notification(`:Alice!u@h PRIVMSG ${target} :hello`, {...config,chantypes:'#&+'}).target, '+local');
    assert.equal(notification(':Me!u@h PRIVMSG Renamed :new owner of old nick', config).target, 'Me');
    assert.equal(notification(':Alice!u@h PRIVMSG PreviousNick :delayed before nick change', config).target, 'Alice');
    assert.equal(notification(':Alice!u@h PRIVMSG Me :\x01VERSION\x01', config), null);
    assert.equal(notification(':Alice!u@h PRIVMSG Me :\x01ACTION waves\x01', config).body, 'Alice waves');
    assert.equal(notification(':Alice!u@h PRIVMSG Me :\x0304red\x0f', config).body, 'red');
});

test('read pushes retain marker timestamp and IRC case mapping', () => {
    const marker = notification('MARKREAD #Chan[ :timestamp=2026-09-20T10:00:00.123Z', config);
    assert.deepEqual(marker, {kind:'read', target:'#chan{', time:Date.parse('2026-09-20T10:00:00.123Z')});
    assert.equal(notification('MARKREAD #chan :timestamp=invalid', config), null);
    assert.equal(fold('A[^', 'ascii'), 'a[^');
    assert.equal(fold('A[^', 'strict-rfc1459'), 'a{^');
});

test('invitations and registration notes are supported without arbitrary commands', () => {
    assert.equal(notification(':Alice!u@h INVITE Me :#chat', config).target, '#chat');
    assert.equal(notification('NOTE WEBPUSH REGISTERED :enabled', config).body, 'Notifications enabled');
    assert.equal(notification('FAIL WEBPUSH INTERNAL_ERROR :secret endpoint', config), null);
});

test('notification navigation stays on application origin and encodes remote targets', () => {
    const scope = 'a'.repeat(64);
    const url = new URL(navigation('https://client.example', scope, '//evil.example/#x'));
    assert.equal(url.origin, 'https://client.example');
    assert.equal(url.pathname, '/');
    assert.equal(new URLSearchParams(url.hash.slice(1)).get('push_target'), '//evil.example/#x');
    assert.equal(navigation('https://client.example', '../other', '#chat'), null);
    assert.equal(navigation('https://client.example', scope, '#bad\nQUIT'), null);
});


test('channel-context messages route and format as channel notifications', () => {
    for (const tag of ['+channel-context', '+draft/channel-context']) for (const command of ['PRIVMSG', 'NOTICE']) {
        const result = notification(`@${tag}=#Room[ :Alice!u@h ${command} Me :hello`, config);
        assert.equal(result.target, '#Room[');
        assert.equal(result.body, 'Alice: hello');
        assert.equal(fold(result.target), notification('MARKREAD #room{ :timestamp=2026-09-20T10:00:00Z', config).target);
    }
    for (const context of ['Alice', '#bad\\sroom', '#one,#two', '#bad\\nroom', '#bad:room', '#' + 'x'.repeat(512)]) {
        assert.equal(notification(`@+draft/channel-context=${context} :Alice!u@h PRIVMSG Me :hello`, config).target, 'Alice');
    }
    assert.equal(notification('@+draft/channel-context=+local :Alice!u@h PRIVMSG Me :hello', {...config, chantypes:'#&+'}).target, '+local');
});


test('channel context never redirects public, status-targeted or server traffic', () => {
    for (const tag of ['+channel-context', '+draft/channel-context']) {
        for (const command of ['PRIVMSG', 'NOTICE']) {
            for (const target of ['#actual', '@#actual', '+#actual']) {
                const result = notification(`@${tag}=#other :Alice!u@h ${command} ${target} :hello`, config);
                assert.equal(result.target, '#actual');
            }
            for (const target of ['*', '$*.example', 'Me,Bob']) {
                assert.equal(notification(`@${tag}=#other :Alice!u@h ${command} ${target} :hello`, config).target, 'Alice');
            }
            assert.equal(notification(`@${tag}=#other :irc.example ${command} Me :hello`, config).target, 'irc.example');
        }
    }
});

test('final channel-context tag takes precedence without falling back from invalid values', () => {
    const both = '@+channel-context=#final;+draft/channel-context=#draft :Alice!u@h PRIVMSG Me :hello';
    assert.equal(notification(both, config).target, '#final');
    for (const context of ['', 'Bob', '#bad\\sroom']) {
        assert.equal(notification(`@+channel-context=${context};+draft/channel-context=#draft :Alice!u@h PRIVMSG Me :hello`, config).target, 'Alice');
    }
});

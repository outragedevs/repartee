export function parseMessage(payload) {
    if (typeof payload !== "string" || payload.length > 8192) return null;
    const line = payload.replace(/\r\n$/, "");
    if (!line || /[\r\n\0]/.test(line)) return null;
    let rest = line;
    const tags = Object.create(null);
    if (rest.startsWith("@")) {
        const end = rest.indexOf(" ");
        if (end < 2) return null;
        for (const item of rest.slice(1, end).split(";")) {
            const split = item.indexOf("=");
            const key = split < 0 ? item : item.slice(0, split);
            const value = split < 0 ? "" : item.slice(split + 1);
            let decoded = "";
            for (let index = 0; index < value.length; index++) {
                if (value[index] !== "\\") { decoded += value[index]; continue; }
                const escaped = value[++index];
                if (escaped !== undefined) decoded += ({s: " ", ":": ";", r: "\r", n: "\n", "\\": "\\"}[escaped] ?? escaped);
            }
            tags[key] = decoded;
        }
        rest = rest.slice(end + 1).trimStart();
    }
    let prefix = "";
    if (rest.startsWith(":")) {
        const end = rest.indexOf(" ");
        if (end < 2) return null;
        prefix = rest.slice(1, end);
        rest = rest.slice(end + 1).trimStart();
    }
    const parts = [];
    while (rest) {
        if (rest.startsWith(":")) { parts.push(rest.slice(1)); break; }
        const end = rest.indexOf(" ");
        if (end < 0) { parts.push(rest); break; }
        parts.push(rest.slice(0, end));
        rest = rest.slice(end + 1).trimStart();
        if (parts.length > 15) return null;
    }
    if (!parts.length || !/^[a-zA-Z]+$/.test(parts[0])) return null;
    return {command: parts[0].toUpperCase(), params: parts.slice(1), prefix, tags};
}

export function fold(value, mapping = "rfc1459") {
    let result = value.replace(/[A-Z]/g, ch => ch.toLowerCase());
    if (mapping !== "ascii") result = result.replace(/[\[\]\\]/g, ch => ({"[": "{", "]": "}", "\\": "|"}[ch]));
    if (mapping === "rfc1459") result = result.replace(/\^/g, "~");
    return result;
}

function plain(value) {
    return value.replace(/\x03(?:\d{1,2}(?:,\d{1,2})?)?/g, "")
        .replace(/\x04(?:[0-9a-fA-F]{6}(?:,[0-9a-fA-F]{6})?)?/g, "")
        .replace(/[\x00-\x1f\x7f]/g, "").slice(0, 1000);
}

export function notification(payload, config, now = Date.now()) {
    const message = parseMessage(payload);
    if (!message) return null;
    const {command, params, prefix, tags} = message;
    const sender = prefix.split(/[!@]/, 1)[0];
    const timestamp = tags.time ? Date.parse(tags.time) : now;
    const time = Number.isFinite(timestamp) ? timestamp : now;
    const mapping = config.casemapping || "rfc1459";
    if (command === "MARKREAD" && params.length === 2 && params[1].startsWith("timestamp=")) {
        const readAt = Date.parse(params[1].slice(10));
        return Number.isFinite(readAt) ? {kind: "read", target: fold(params[0], mapping), time: readAt} : null;
    }
    if (command === "NOTE" && params[0] === "WEBPUSH" && params[1] === "REGISTERED") {
        return {kind: "show", target: "", title: config.label, body: "Notifications enabled", time, msgid: "registration"};
    }
    let target;
    let body;
    if ((command === "PRIVMSG" || command === "NOTICE") && params.length === 2 && sender) {
        let recipient = params[0];
        let withoutStatus = recipient;
        while (withoutStatus && (config.statusmsg || "").includes(withoutStatus[0])) {
            withoutStatus = withoutStatus.slice(1);
            if (withoutStatus && (config.chantypes || "#&").includes(withoutStatus[0])) recipient = withoutStatus;
        }
        const context = tags["+draft/channel-context"];
        if (context && context.length <= 512 && !/[\x00-\x20\x7f,:]/.test(context) && (config.chantypes || "#&").includes(context[0])) recipient = context;
        const channel = recipient && (config.chantypes || "#&").includes(recipient[0]);
        target = channel ? recipient : sender;
        body = params[1];
        if (body.startsWith("\x01")) {
            if (!body.startsWith("\x01ACTION ") || !body.endsWith("\x01")) return null;
            body = `${sender} ${body.slice(8, -1)}`;
        } else if (channel) body = `${sender}: ${body}`;
    } else if (command === "INVITE" && params.length === 2 && sender) {
        target = params[1];
        body = `${sender} invited you to ${target}`;
    } else return null;
    return {kind: "show", target, title: `${plain(target)} · ${plain(config.label)}`, body: plain(body), time, msgid: tags.msgid || ""};
}

export function navigation(origin, scope, target) {
    if (!/^[a-f0-9]{64}$/.test(scope) || typeof target !== "string" || /[\x00-\x20\x7f]/.test(target) || target.length > 512) return null;
    const url = new URL("/", origin);
    url.hash = new URLSearchParams({push_scope: scope, push_target: target}).toString();
    return url.href;
}

meta = {
    name = "keepnick",
    version = "1.0",
    description = "Reclaim a preferred nick when it becomes available",
}

function setup(api)
    local wanted = {}
    local held = {}
    local pending_manual = {}
    local pending_ison = {}
    local attempting = {}
    local tick = 0

    local function fold(nick)
        return nick:lower():gsub("%[", "{"):gsub("%]", "}"):gsub("\\", "|"):gsub("%^", "~")
    end

    local function same(a, b)
        return a ~= nil and b ~= nil and fold(a) == fold(b)
    end

    local function say(message)
        api.ui.print("Keepnick: " .. message)
    end

    for line in (api.config.get("nicks", "") .. "\n"):gmatch("([^\n]*)\n") do
        local id, nick = line:match("^([^\t]+)\t([^\t]+)$")
        if id and nick then
            wanted[id] = nick
        end
    end

    local function save()
        local ids = {}
        for id in pairs(wanted) do
            ids[#ids + 1] = id
        end
        table.sort(ids)
        local lines = {}
        for _, id in ipairs(ids) do
            lines[#lines + 1] = id .. "\t" .. wanted[id]
        end
        api.config.set("nicks", table.concat(lines, "\n"))
    end

    local function connection(id)
        local info = api.store.connection(id)
        if info and info.connected then
            return info
        end
        return nil
    end

    local function resolve(name, current_id)
        if not name then
            return current_id
        end
        for _, info in ipairs(api.store.connections()) do
            if same(info.id, name) or same(info.label, name) then
                return info.id
            end
        end
        return nil
    end

    local function query(id, current_nick)
        local nick = wanted[id]
        if not nick or held[id] or same(current_nick, nick) then
            pending_ison[id] = nil
            attempting[id] = nil
            return
        end
        local pending = pending_ison[id]
        if pending and tick - pending.tick < 2 then
            return
        end
        pending_ison[id] = { nick = nick, tick = tick }
        api.irc.raw("ISON :" .. nick, id)
    end

    local function try_nick(id)
        local nick = wanted[id]
        if nick and not held[id] and not attempting[id] then
            local info = connection(id)
            if info and not same(info.nick, nick) then
                attempting[id] = true
                api.irc.nick(nick, id)
            end
        end
    end

    local function check_all()
        tick = tick + 1
        for id in pairs(wanted) do
            local info = connection(id)
            if info then
                attempting[id] = nil
                query(id, info.nick)
            else
                pending_ison[id] = nil
                attempting[id] = nil
            end
        end
    end

    local function parse_keepnick(args)
        local network
        local nick
        local i = 1
        while i <= #args do
            local arg = args[i]
            if arg == "-net" or arg == "-network" or arg == "-chatnet" then
                i = i + 1
                network = args[i]
                if not network then
                    return nil, nil, "Missing network after " .. arg
                end
            elseif arg:sub(1, 1) == "-" then
                return nil, nil, "Unknown option " .. arg
            elseif nick then
                return nil, nil, "Usage: /keepnick [-net <network>] [nick]"
            else
                nick = arg
            end
            i = i + 1
        end
        return network, nick
    end

    api.command("keepnick", {
        description = "Keep trying to reclaim a nickname",
        usage = "/keepnick [-net <network>] [nick]",
        handler = function(args, current_id)
            local network, nick, problem = parse_keepnick(args)
            if problem then
                say(problem)
                return
            end
            local id = resolve(network, current_id)
            if not id then
                say(network and ("Unknown network: " .. network) or "Select a network first")
                return
            end
            local info = connection(id)
            nick = nick or (info and info.nick)
            if not nick then
                say("A nick is required for a disconnected network")
                return
            end
            if nick:find("[%s,:]") then
                say("Invalid nick: " .. nick)
                return
            end
            wanted[id] = nick
            held[id] = nil
            pending_ison[id] = nil
            attempting[id] = nil
            save()
            say("Keeping " .. nick .. " on " .. id)
            if info then
                query(id, info.nick)
            end
        end,
    })

    api.command("unkeepnick", {
        description = "Stop reclaiming a nickname",
        usage = "/unkeepnick [network]",
        handler = function(args, current_id)
            if #args > 1 then
                say("Usage: /unkeepnick [network]")
                return
            end
            local id = resolve(args[1], current_id)
            if not id then
                say("Unknown network")
                return
            end
            if not wanted[id] then
                say("No nick kept on " .. id)
                return
            end
            wanted[id] = nil
            held[id] = nil
            pending_ison[id] = nil
            attempting[id] = nil
            save()
            say("Stopped keeping a nick on " .. id)
        end,
    })

    api.command("listnick", {
        description = "List preferred nicknames",
        usage = "/listnick",
        handler = function()
            local ids = {}
            for id in pairs(wanted) do
                ids[#ids + 1] = id
            end
            table.sort(ids)
            if #ids == 0 then
                say("No nicks configured")
                return
            end
            for _, id in ipairs(ids) do
                say(id .. ": " .. wanted[id] .. (held[id] and " (inactive)" or " (active)"))
            end
        end,
    })

    api.on("command_input", function(event)
        if event.command ~= "nick" or not event.connection_id then
            return
        end
        local nick = type(event.args) == "table" and event.args[1]
            or type(event.args) == "string" and event.args:match("^%s*(%S+)")
        local id = event.connection_id
        if nick and wanted[id] and same(api.store.our_nick(id), wanted[id]) then
            pending_manual[id] = nick
        end
    end)

    api.on("irc.nick", function(event)
        local id = event.connection_id
        local nick = wanted[id]
        if not nick then
            return
        end
        local own = api.store.our_nick(id)
        if same(event.nick, own) then
            attempting[id] = nil
            pending_ison[id] = nil
            if same(event.new_nick, nick) then
                if held[id] then
                    say("Resuming on " .. id)
                else
                    say("Got " .. nick .. " back on " .. id)
                end
                held[id] = nil
            elseif pending_manual[id] and same(event.new_nick, pending_manual[id])
                and same(event.nick, nick) then
                held[id] = true
                say("Paused on " .. id .. " after manual /nick")
            else
                query(id, event.new_nick)
            end
            pending_manual[id] = nil
        elseif same(event.nick, nick) then
            try_nick(id)
        end
    end)

    api.on("irc.quit", function(event)
        local id = event.connection_id
        if same(event.nick, wanted[id]) then
            try_nick(id)
        end
    end)

    api.on("irc.ison", function(event)
        local id = event.connection_id
        local pending = pending_ison[id]
        if not pending then
            return
        end
        pending_ison[id] = nil
        if not same(pending.nick, wanted[id]) or held[id] then
            return true
        end
        for online in (event.nicks or ""):gmatch("%S+") do
            if same(online, pending.nick) then
                return true
            end
        end
        try_nick(id)
        return true
    end)

    api.on("connected", function(event)
        pending_ison[event.connection_id] = nil
        attempting[event.connection_id] = nil
        query(event.connection_id, event.nick)
    end)

    api.on("disconnected", function(event)
        pending_ison[event.connection_id] = nil
        attempting[event.connection_id] = nil
        pending_manual[event.connection_id] = nil
    end)

    local timer = api.timer(12000, check_all)
    api.timeout(1000, check_all)

    return function()
        api.cancel_timer(timer)
    end
end

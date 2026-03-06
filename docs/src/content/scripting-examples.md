# Scripting — Examples

Practical Lua script examples for repartee.

## Auto-greet on join

Greet users when they join a specific channel:

```lua
meta = {
    name = "autogreet",
    version = "1.0",
    description = "Auto-greet users on join"
}

function setup(api)
    local greet_channels = { ["#mychannel"] = true }

    api.on("irc.join", function(event)
        if greet_channels[event.channel] and event.nick ~= api.our_nick() then
            api.irc.say(event.channel, "Welcome, " .. event.nick .. "!")
        end
    end)
end
```

## URL logger

Log all URLs posted to channels:

```lua
meta = {
    name = "urllogger",
    version = "1.0",
    description = "Log URLs from messages"
}

function setup(api)
    local urls = {}

    api.on("irc.privmsg", function(event)
        for url in event.message:gmatch("https?://[%w%.%-/%%?&=_#]+") do
            table.insert(urls, {
                time = os.date("%H:%M"),
                nick = event.nick,
                channel = event.target,
                url = url
            })
            api.log("URL: " .. url .. " from " .. event.nick)
        end
    end)

    api.command("urls", {
        handler = function(args)
            local count = tonumber(args[1]) or 10
            local start = math.max(1, #urls - count + 1)
            for i = start, #urls do
                local u = urls[i]
                api.print(u.time .. " " .. u.nick .. " > " .. u.url)
            end
        end,
        description = "Show recent URLs",
        usage = "/urls [count]"
    })
end
```

## Highlight monitor

Copy highlighted messages to a dedicated buffer:

```lua
meta = {
    name = "hilight",
    version = "1.0",
    description = "Monitor highlighted messages"
}

function setup(api)
    api.on("irc.privmsg", function(event)
        local my_nick = api.our_nick()
        if my_nick and event.message:lower():find(my_nick:lower(), 1, true) then
            local msg = "[" .. event.target .. "] <" .. event.nick .. "> " .. event.message
            api.print(msg)
        end
    end)
end
```

## Custom slap command

The classic IRC `/slap` command:

```lua
meta = {
    name = "slap",
    version = "1.0",
    description = "Slap someone with a large trout"
}

function setup(api)
    local items = {
        "a large trout",
        "a mass of wet noodles",
        "a mass-produced plastic toy",
        "a mass of jello",
        "a mass of cotton candy",
    }

    api.command("slap", {
        handler = function(args)
            local target = args[1]
            if not target then
                api.print("Usage: /slap <nick>")
                return
            end
            local item = items[math.random(#items)]
            local buf = api.active_buffer()
            if buf then
                api.irc.action(buf, "slaps " .. target .. " around a bit with " .. item)
            end
        end,
        description = "Slap someone with a random object",
        usage = "/slap <nick>"
    })
end
```

## Spam filter

Block messages matching patterns:

```lua
meta = {
    name = "spamfilter",
    version = "1.0",
    description = "Filter spam messages"
}

function setup(api)
    local patterns = {
        "buy cheap",
        "free bitcoins",
        "click here now",
    }

    -- High priority so we run before default handlers
    api.on("irc.privmsg", function(event)
        local lower = event.message:lower()
        for _, pattern in ipairs(patterns) do
            if lower:find(pattern, 1, true) then
                api.log("Blocked spam from " .. event.nick .. ": " .. event.message)
                return true  -- suppress the event
            end
        end
    end, 90)  -- priority 90 = high
end
```

## Nick highlight with sound

Play a terminal bell when your nick is mentioned:

```lua
meta = {
    name = "nickbell",
    version = "1.0",
    description = "Bell on nick mention"
}

function setup(api)
    api.on("irc.privmsg", function(event)
        local my_nick = api.our_nick()
        if my_nick and event.message:lower():find(my_nick:lower(), 1, true) then
            io.write("\a")  -- terminal bell
        end
    end)
end
```

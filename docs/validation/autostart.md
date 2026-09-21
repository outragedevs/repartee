# Multi-network startup

Each configured autoconnect entry starts an independent connection attempt. The
configuration uses a map, so file order is not a connection-completion order.
Every attempt retains its own connection ID and generation; stale events from a
cancelled attempt are discarded.

Registration negotiates CAP/SASL and reports the welcome numeric to the app.
The app runs that connection's `autosendcmd` in its server-buffer context, then
restores the user's selected buffer. Nick substitutions use that connection's
nick. Configuration lookup uses the connection ID, never another network's
matching display label.

When startup commands are configured, the reader waits for the app to finish
queueing them before reading subsequent messages. This keeps the library's
end-of-MOTD autojoin behind those commands on the same outgoing connection.
This orders writes; it does not wait for successful OPER/NickServ authentication.
`WAIT` delays in `autosendcmd` remain unsupported and are skipped.

The IRC library batches configured JOINs, puts keyed channels first and splits
at the protocol line limit. The app's rejoin list contains channel names only,
so a `#channel key` configuration entry cannot become an extra malformed JOIN.

Regression coverage uses synthetic credentials and three independent localhost
TCP servers, with 60 channels per network, a keyed channel and multiple JOIN
batches. Both early welcome and explicit CAP rejection are exercised. The
application event loop deliberately starts consuming events after all welcome
bursts arrive. The test verifies the command destination, command-before-JOIN
ordering, complete channel coverage, keys, line lengths and unchanged focus.
A capturing-sender regression also verifies that an unknown connection cannot
fall back to the active network.

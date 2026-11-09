# When things go wrong

Trading systems fail in the gaps between "sent" and "confirmed". Here is what this one does.

| What happens | What the system does |
|---|---|
| **An order times out** | Looks it up by its own ID. It never resends blindly, and the stock stays blocked until the answer is known |
| **A fill is reported twice** | Counts it once, matched by trade ID |
| **A fill arrives for an unknown order** | Applies it anyway, because the position is real, and the clock closes it later |
| **The broker rejects an exit** | Tries again on the next bar or clock tick |
| **The stop fires while we're exiting** | Cancels the stop first and sells only after that's confirmed, so we can't sell twice |
| **A cancel fails** | Looks the order up, and retries if it's still live |
| **The broker's stop doesn't work** | If a bar closes past the stop, the strategy exits itself |
| **The data feed goes down** | No new entries. Exits still work |
| **One stock's bars stop arriving** | Square-off runs from the clock, so it still gets closed |
| **We lose ₹10,000 in a day** | No new trades, and everything is closed |
| **The program restarts mid-day** | Positions and today's P&L are rebuilt from the broker, so the loss limit can't reset. Open trades keep their broker stops and close at square-off |
| **A bad bar arrives** | Refused without changing any state. Missing opening bars mean no ORB trade that day |
| **The login token expires** | A new one is issued automatically. A wrong PIN stops immediately, to avoid locking the account |
| **A config has a typo** | Start-up fails. It won't misbehave at 09:30 |

There is also a manual kill switch: no new trades, and everything open is closed.

## How this was tested

The simulated broker injects rejections, timeouts, failed cancels and duplicate fills. After each run, three separate sets of books must match to the paisa: the trade journal, the risk manager and the broker's own records.

!!! note "Honest limits"
    All of this is tested against a simulated broker. The real Dhan API hasn't been connected yet, and real brokers will have surprises of their own. That's what the paper-trading stage is for.

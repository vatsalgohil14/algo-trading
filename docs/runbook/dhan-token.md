# Dhan access token

Tokens last 24 hours. With a PIN and TOTP secret set, the system issues new ones itself.

## Setup (once)

1. On Dhan Web, open **DhanHQ Trading APIs → Setup TOTP** and confirm with the OTP.
2. Save the text code shown beside the QR code. That is the TOTP secret. Add it to your authenticator app too, and enter the first code to finish.
3. Set these in your shell. Never put them in chat or in a tracked file:

| Variable | Value |
|---|---|
| `DHAN_CLIENT_ID` | Your client id |
| `DHAN_PIN` | Your Dhan PIN |
| `DHAN_TOTP_SECRET` | The text code from step 2 |
| `DHAN_TOKEN_CACHE` | Optional; default `secrets/dhan_token.json` |

## Check it

```bash
cargo run --release -p tools --bin dhan-token
```

It gets or reuses a token, confirms that Dhan accepts it, and prints when it expires. It never prints the token.

## How it works

- The token is saved in `secrets/` (gitignored), readable only by you, so restarts reuse it.
- A token must last to 16:00 IST on the next session day, so a running process renews it just after 16:00. A failure then leaves hours to retry.
- Network and server errors are retried with growing gaps, up to every 30 minutes.
- If Dhan refuses the request, for example because the PIN is wrong, nothing more is tried, since repeated wrong attempts could lock the account. Fix the setting and restart.

## Security

The PIN and TOTP secret together can issue tokens for your account, and so place trades. On AWS, keep them in a secrets manager. If they leak, reset the PIN and set up TOTP again.

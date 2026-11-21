# Data quality

What the 1-minute history from Dhan contains, what was wrong with it, and what was done. Checked on 2026-11-19 across all 50 Nifty stocks, about 23 million candles from 1 December 2021.

## Removed: mock trading days

NSE runs mock trading sessions on some Saturdays to test systems. Dhan's history includes three of them as if they were real:

| Day | Candles | Why it is fake |
|---|---|---|
| Sat 5 Feb 2022 | 1,623 | Median day range 15.5%; reopened 8.5% away on Monday |
| Sat 9 Apr 2022 | 11,188 | Median day range 22.2%; reopened 7.1% away |
| Sat 30 Apr 2022 | 7,250 | Median day range 19.9%; reopened 5.8% away |

All three days were dropped from `bars_1m`. (The 1-minute downloader, `dhan-history`, has since been deleted with the intraday code; the table stays.)

## Kept: real weekend sessions

These weekend days are real trading. Their prices continue from the days around them, with median gaps under 1% and normal day ranges:

| Day | Session |
|---|---|
| Sun 12 Nov 2023 | Diwali Muhurat |
| Sat 20 Jan 2024 | Full session |
| Sat 2 Mar 2024 | Special session |
| Sat 18 May 2024 | Special session |
| Sat 1 Feb 2025 | Budget day |
| Sun 1 Feb 2026 | Budget day |
| Sun 8 Nov 2026 | Diwali Muhurat |

Never filter history by weekday alone.

## Removed: candles outside the session

| Candles | Count | Until |
|---|---|---|
| 09:07 pre-open candle | 8,195 | Dec 2023 |
| 15:30 and later, almost all flat with zero volume | 168,786 | Nov 2024 |

Neither is continuous trading. Both were deleted from `bars_1m` on 21 Nov 2026, with the 4 invalid candles below. `bars_1m` now holds 22,790,047 candles.

## Known, not fixed yet

- **Adjusted for corporate actions only in part.** Dhan adjusts most past prices for later splits and bonuses: RELIANCE shows about ₹1,090 and NESTLEIND about ₹935 in February 2022, well below what they traded at then. Some actions still show as price jumps, for example BSE on 21 Mar 2022 (937 → 300) and TMPV on 14 Oct 2025 (665 → 400, the Tata Motors demerger). Indicators that span such a date (moving averages, ATR, previous-day levels) are wrong around it. Adjusted prices also shift price filters: a stock can pass a ₹5,000 cap in the past only because of a later split. Fixing this needs a list of corporate actions to check each stock against.
- **Flat zero-volume candles inside the session.** 20,849 of them, mostly at 15:29 in 2022. They are filler, and they pull volume averages down slightly.
- **Invalid candles, deleted.** HINDUNILVR at 15:29 on 22 Apr 2025 closed below its own low, and three NESTLEIND 15:29 candles in 2022 had negative volume. All four were deleted on 21 Nov 2026; readers had always rejected them.
- **Short history.** JIOFIN starts on 21 Aug 2023, so it has about 800 days against about 1,236 for the others.
- **Real gaps.** Most other large jumps are real overnight gaps at the open, such as 3 Jun 2024 (election results eve) and 7 Apr 2025 (the tariff sell-off).

## Checking again

The queries used are below. Run them in the QuestDB console after any new download.

Weekend days present, to compare with the tables above:

```sql
SELECT timestamp_floor('d', ts) day, day_of_week(ts) dow, count() candles
FROM bars_1m
WHERE day_of_week(ts) IN (6, 7)
GROUP BY day, dow
ORDER BY day;
```

Jumps of more than 8% between consecutive candles:

```sql
SELECT * FROM (
  SELECT ts, symbol, open, lag(close) OVER (PARTITION BY symbol ORDER BY ts) prev_close
  FROM bars_1m
) WHERE prev_close > 0 AND abs(open / prev_close - 1) > 0.08
ORDER BY ts;
```

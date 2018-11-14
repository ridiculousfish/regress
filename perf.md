These are perf results from 5/24/20, from [regex-performance](github.com/rust-leipzig/regex-performance.git).

```
'../3200.txt' loaded. (Length: 16013977 bytes)
-----------------
Regex: 'Twain'
[      pcre] time:     1.6 ms (+/-  4.8 %), matches:      811
[  pcre-dfa] time:     8.5 ms (+/-  0.6 %), matches:      811
[  pcre-jit] time:     9.9 ms (+/-  0.8 %), matches:      811
[       re2] time:     1.1 ms (+/-  5.3 %), matches:      811
[      onig] time:    10.9 ms (+/-  0.5 %), matches:      811
[rust_regex] time:     1.1 ms (+/-  3.3 %), matches:      811
[   regress] time:     5.6 ms (+/-  0.9 %), matches:      811
-----------------
Regex: '(?i)Twain'
[      pcre] time:    28.5 ms (+/-  0.4 %), matches:      965
[  pcre-dfa] time:    40.8 ms (+/-  0.6 %), matches:      965
[  pcre-jit] time:    10.1 ms (+/-  0.7 %), matches:      965
[       re2] time:    32.1 ms (+/-  0.4 %), matches:      965
[      onig] time:    45.9 ms (+/-  0.3 %), matches:      965
[rust_regex] time:    10.3 ms (+/-  0.5 %), matches:      965
[   regress] time:    21.6 ms (+/-  0.7 %), matches:      965
-----------------
Regex: '[a-z]shing'
[      pcre] time:   183.3 ms (+/-  0.6 %), matches:     1540
[  pcre-dfa] time:   312.1 ms (+/-  0.1 %), matches:     1540
[  pcre-jit] time:     9.3 ms (+/-  0.7 %), matches:     1540
[       re2] time:    52.0 ms (+/-  0.5 %), matches:     1540
[      onig] time:     9.3 ms (+/-  0.6 %), matches:     1540
[rust_regex] time:     3.4 ms (+/-  2.1 %), matches:     1540
[   regress] time:   128.0 ms (+/-  0.1 %), matches:     1540
-----------------
Regex: 'Huck[a-zA-Z]+|Saw[a-zA-Z]+'
[      pcre] time:    10.3 ms (+/-  0.7 %), matches:      262
[  pcre-dfa] time:    10.8 ms (+/-  0.9 %), matches:      262
[  pcre-jit] time:     1.4 ms (+/-  3.3 %), matches:      262
[       re2] time:    23.0 ms (+/-  0.3 %), matches:      262
[      onig] time:    19.1 ms (+/-  0.4 %), matches:      262
[rust_regex] time:     1.4 ms (+/-  3.7 %), matches:      262
[   regress] time:     7.8 ms (+/-  0.6 %), matches:      262
-----------------
Regex: '\b\w+nn\b'
[      pcre] time:   269.4 ms (+/-  0.5 %), matches:      262
[  pcre-dfa] time:   435.1 ms (+/-  0.1 %), matches:      262
[  pcre-jit] time:    51.3 ms (+/-  0.3 %), matches:      262
[       re2] time:    19.8 ms (+/-  1.5 %), matches:      262
[      onig] time:   333.6 ms (+/-  0.3 %), matches:      262
[rust_regex] time:   101.5 ms (+/-  0.5 %), matches:      262
[   regress] time:   195.5 ms (+/-  0.1 %), matches:      262
-----------------
Regex: '[a-q][^u-z]{13}x'
[      pcre] time:   230.1 ms (+/-  0.1 %), matches:     4094
[  pcre-dfa] time:   842.2 ms (+/-  0.1 %), matches:     4094
[  pcre-jit] time:     1.0 ms (+/-  7.5 %), matches:     4094
[       re2] time:   102.8 ms (+/-  7.9 %), matches:     4094
[      onig] time:    22.9 ms (+/-  1.3 %), matches:     4094
[rust_regex] time:  1666.2 ms (+/-  1.6 %), matches:     4094
[   regress] time:   382.8 ms (+/-  0.1 %), matches:     4094
-----------------
Regex: 'Tom|Sawyer|Huckleberry|Finn'
[      pcre] time:    13.3 ms (+/-  1.6 %), matches:     2598
[  pcre-dfa] time:    14.3 ms (+/-  0.8 %), matches:     2598
[  pcre-jit] time:    14.9 ms (+/-  0.3 %), matches:     2598
[       re2] time:    23.9 ms (+/-  0.7 %), matches:     2598
[      onig] time:    22.0 ms (+/-  0.4 %), matches:     2598
[rust_regex] time:    24.8 ms (+/-  0.3 %), matches:     2598
[   regress] time:    11.5 ms (+/-  1.2 %), matches:     2598
-----------------
Regex: '(?i)Tom|Sawyer|Huckleberry|Finn'
[      pcre] time:   134.6 ms (+/-  0.1 %), matches:     4152
[  pcre-dfa] time:   163.5 ms (+/-  0.3 %), matches:     4152
[  pcre-jit] time:    40.8 ms (+/-  0.5 %), matches:     4152
[       re2] time:    48.1 ms (+/-  0.1 %), matches:     4152
[      onig] time:   142.0 ms (+/-  0.2 %), matches:     4152
[rust_regex] time:    25.3 ms (+/-  0.6 %), matches:     4152
[   regress] time:   113.3 ms (+/-  1.7 %), matches:     4152
-----------------
Regex: '.{0,2}(Tom|Sawyer|Huckleberry|Finn)'
[      pcre] time:  1707.2 ms (+/-  0.0 %), matches:     2598
[  pcre-dfa] time:  1517.7 ms (+/-  0.1 %), matches:     2598
[  pcre-jit] time:   130.4 ms (+/-  0.2 %), matches:     2598
[       re2] time:    22.3 ms (+/-  0.7 %), matches:     2598
[      onig] time:    40.6 ms (+/-  0.4 %), matches:     2598
[rust_regex] time:    21.4 ms (+/-  1.2 %), matches:     2598
[   regress] time:   969.3 ms (+/-  0.0 %), matches:     2598
-----------------
Regex: '.{2,4}(Tom|Sawyer|Huckleberry|Finn)'
[      pcre] time:  1664.6 ms (+/-  0.0 %), matches:     1976
[  pcre-dfa] time:  1807.6 ms (+/-  0.0 %), matches:     1976
[  pcre-jit] time:   142.3 ms (+/-  0.3 %), matches:     1976
[       re2] time:    22.3 ms (+/-  0.6 %), matches:     1976
[      onig] time:    38.9 ms (+/-  0.3 %), matches:     1976
[rust_regex] time:    21.4 ms (+/-  1.5 %), matches:     1976
[   regress] time:  1024.4 ms (+/-  0.8 %), matches:     1976
-----------------
Regex: 'Tom.{10,25}river|river.{10,25}Tom'
[      pcre] time:    27.8 ms (+/-  0.8 %), matches:        2
[  pcre-dfa] time:    35.0 ms (+/-  0.5 %), matches:        2
[  pcre-jit] time:     8.3 ms (+/-  1.1 %), matches:        2
[       re2] time:    27.9 ms (+/-  2.3 %), matches:        2
[      onig] time:    37.7 ms (+/-  0.5 %), matches:        2
[rust_regex] time:     8.5 ms (+/-  7.0 %), matches:        2
[   regress] time:    18.7 ms (+/-  0.6 %), matches:        2
-----------------
Regex: '[a-zA-Z]+ing'
[      pcre] time:   399.2 ms (+/-  0.1 %), matches:    78424
[  pcre-dfa] time:   718.4 ms (+/-  0.0 %), matches:    78424
[  pcre-jit] time:    44.5 ms (+/-  0.6 %), matches:    78424
[       re2] time:    59.0 ms (+/-  0.2 %), matches:    78424
[      onig] time:   341.2 ms (+/-  0.1 %), matches:    78424
[rust_regex] time:     8.8 ms (+/-  0.5 %), matches:    78424
[   regress] time:   300.0 ms (+/-  0.1 %), matches:    78424
-----------------
Regex: '\s[a-zA-Z]{0,12}ing\s'
[      pcre] time:   181.7 ms (+/-  0.2 %), matches:    55248
[  pcre-dfa] time:   278.7 ms (+/-  0.2 %), matches:    55248
[  pcre-jit] time:    56.2 ms (+/-  0.3 %), matches:    55248
[       re2] time:    33.2 ms (+/-  0.7 %), matches:    55248
[      onig] time:    37.7 ms (+/-  0.4 %), matches:    55248
[rust_regex] time:    24.7 ms (+/-  0.7 %), matches:    55248
[   regress] time:   153.1 ms (+/-  0.1 %), matches:    55248
-----------------
Regex: '([A-Za-z]awyer|[A-Za-z]inn)\s'
[      pcre] time:   378.3 ms (+/-  0.1 %), matches:      209
[  pcre-dfa] time:   494.2 ms (+/-  0.1 %), matches:      209
[  pcre-jit] time:    20.3 ms (+/-  1.0 %), matches:      209
[       re2] time:    49.8 ms (+/-  0.3 %), matches:      209
[      onig] time:    90.5 ms (+/-  0.5 %), matches:      209
[rust_regex] time:    21.1 ms (+/-  0.4 %), matches:      209
[   regress] time:   228.5 ms (+/-  0.1 %), matches:      209
-----------------
Regex: '["'][^"']{0,30}[?!\.]["']'
[      pcre] time:    25.0 ms (+/-  1.0 %), matches:     8886
[  pcre-dfa] time:    37.2 ms (+/-  0.4 %), matches:     8886
[  pcre-jit] time:     5.6 ms (+/-  1.9 %), matches:     8886
[       re2] time:    24.7 ms (+/-  0.6 %), matches:     8886
[      onig] time:    35.0 ms (+/-  0.3 %), matches:     8886
[rust_regex] time:     5.7 ms (+/-  5.4 %), matches:     8886
[   regress] time:    24.9 ms (+/-  0.5 %), matches:     8886
-----------------
Regex: '∞|✓'
[      pcre] time:     0.5 ms (+/-  9.5 %), matches:        2
[  pcre-dfa] time:     7.1 ms (+/-  0.7 %), matches:        2
[  pcre-jit] time:     0.8 ms (+/-  7.8 %), matches:        2
[       re2] time:     0.5 ms (+/-  8.0 %), matches:        0
[      onig] time:    21.2 ms (+/-  0.6 %), matches:        2
[rust_regex] time:    24.7 ms (+/-  0.4 %), matches:        2
[   regress] time:     5.9 ms (+/-  2.4 %), matches:        2
-----------------
Total Results:
[      pcre] time:   5255.3 ms, score:     11 points,
[  pcre-dfa] time:   6723.1 ms, score:      3 points,
[  pcre-jit] time:    547.1 ms, score:     48 points,
[       re2] time:    542.4 ms, score:     41 points,
[      onig] time:   1248.4 ms, score:     17 points,
[rust_regex] time:   1970.4 ms, score:     58 points,
[   regress] time:   3590.8 ms, score:     14 points,
```

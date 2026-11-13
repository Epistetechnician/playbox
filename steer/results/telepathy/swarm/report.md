### qwen8b -> olmo2  (big, NVIDIA H100 80GB HBM3, bfloat16; map cosine 0.8705, 6 agents, k in [2, 3], 100 trials per k)
**qwen8b -> olmo2, all k** (n = 200 trials)
| arm | mean recall % | all concepts recovered % (95% CI) | rare concepts found % | dominant found % | precision % |
|---|---|---|---|---|---|
| none | 9.8 | 0.5 (0-3) | 11.2 | 8.0 | 12.5 |
| text_words | 80.6 | 54.0 (47-61) | 75.0 | 89.0 | 81.1 |
| latent_one | 43.6 | 2.0 (1-5) | 16.8 | 78.5 | 73.5 |
| latent_all | 45.2 | 4.5 (2-8) | 8.2 | 97.0 | 69.2 |
| latent_random | 47.0 | 6.5 (4-11) | 19.5 | 85.5 | 67.9 |
| latent_novelty | 51.4 | 15.0 (11-21) | 48.8 | 54.5 | 64.5 |
| latent_novelty_x | 31.2 | 8.0 (5-13) | 26.0 | 36.5 | 50.8 |
| direct | 61.2 | 27.0 (21-34) | 61.0 | 62.0 | 68.4 |
| null | 6.5 | 0.5 (0-3) | 8.5 | 3.5 | 9.4 |
| contrast (recall) | first wins | second wins | exact p |
|---|---|---|---|
| latent_novelty vs latent_all | 53 | 29 | 0.0106 |
| latent_novelty vs latent_random | 49 | 31 | 0.0567 |
| latent_novelty vs latent_one | 60 | 25 | 0.000187 |
| latent_all vs latent_one | 18 | 9 | 0.122 |
| latent_novelty_x vs latent_novelty | 12 | 91 | 3.46e-16 |
| latent_novelty vs none | 153 | 6 | 5.81e-38 |
| latent_novelty vs null | 163 | 3 | 1.63e-44 |
| text_words vs latent_novelty | 121 | 14 | 1.98e-22 |
| direct vs latent_novelty | 78 | 35 | 6.45e-05 |
One agent from every true concept was picked in 98.5 (96-99)% of trials by the novelty filter and in 32.0 (26-39)% by a random choice of k agents.
**qwen8b -> olmo2, k = 2** (n = 100 trials)
| arm | mean recall % | all concepts recovered % (95% CI) | rare concepts found % | dominant found % | precision % |
|---|---|---|---|---|---|
| none | 9.0 | 1.0 (0-5) | 11.0 | 7.0 | 9.0 |
| text_words | 89.5 | 79.0 (70-86) | 84.0 | 95.0 | 89.5 |
| latent_one | 51.5 | 4.0 (2-10) | 14.0 | 89.0 | 75.0 |
| latent_all | 52.5 | 8.0 (4-15) | 8.0 | 97.0 | 72.0 |
| latent_random | 55.0 | 12.0 (7-20) | 22.0 | 88.0 | 72.0 |
| latent_novelty | 57.5 | 24.0 (17-33) | 53.0 | 62.0 | 67.0 |
| latent_novelty_x | 45.5 | 16.0 (10-24) | 35.0 | 56.0 | 63.5 |
| direct | 70.0 | 44.0 (35-54) | 71.0 | 69.0 | 75.5 |
| null | 6.0 | 1.0 (0-5) | 8.0 | 4.0 | 7.5 |
| contrast (recall) | first wins | second wins | exact p |
|---|---|---|---|
| latent_novelty vs latent_all | 19 | 10 | 0.136 |
| latent_novelty vs latent_random | 19 | 14 | 0.487 |
| latent_novelty vs latent_one | 23 | 11 | 0.0576 |
| latent_all vs latent_one | 6 | 4 | 0.754 |
| latent_novelty_x vs latent_novelty | 8 | 30 | 0.000472 |
| latent_novelty vs none | 80 | 0 | 1.65e-24 |
| latent_novelty vs null | 86 | 1 | 1.14e-24 |
| text_words vs latent_novelty | 63 | 7 | 2.28e-12 |
| direct vs latent_novelty | 37 | 12 | 0.00047 |
One agent from every true concept was picked in 100.0 (96-100)% of trials by the novelty filter and in 38.0 (29-48)% by a random choice of k agents.
**qwen8b -> olmo2, k = 3** (n = 100 trials)
| arm | mean recall % | all concepts recovered % (95% CI) | rare concepts found % | dominant found % | precision % |
|---|---|---|---|---|---|
| none | 10.7 | 0.0 (0-4) | 11.5 | 9.0 | 16.0 |
| text_words | 71.7 | 29.0 (21-39) | 66.0 | 83.0 | 72.7 |
| latent_one | 35.7 | 0.0 (0-4) | 19.5 | 68.0 | 72.0 |
| latent_all | 38.0 | 1.0 (0-5) | 8.5 | 97.0 | 66.3 |
| latent_random | 39.0 | 1.0 (0-5) | 17.0 | 83.0 | 63.8 |
| latent_novelty | 45.3 | 6.0 (3-12) | 44.5 | 47.0 | 62.0 |
| latent_novelty_x | 17.0 | 0.0 (0-4) | 17.0 | 17.0 | 38.2 |
| direct | 52.3 | 10.0 (6-17) | 51.0 | 55.0 | 61.3 |
| null | 7.0 | 0.0 (0-4) | 9.0 | 3.0 | 11.3 |
| contrast (recall) | first wins | second wins | exact p |
|---|---|---|---|
| latent_novelty vs latent_all | 34 | 19 | 0.0534 |
| latent_novelty vs latent_random | 30 | 17 | 0.0789 |
| latent_novelty vs latent_one | 37 | 14 | 0.00177 |
| latent_all vs latent_one | 12 | 5 | 0.143 |
| latent_novelty_x vs latent_novelty | 4 | 61 | 3.92e-14 |
| latent_novelty vs none | 73 | 6 | 1e-15 |
| latent_novelty vs null | 77 | 2 | 1.05e-20 |
| text_words vs latent_novelty | 58 | 7 | 4.27e-11 |
| direct vs latent_novelty | 41 | 23 | 0.0328 |
One agent from every true concept was picked in 97.0 (92-99)% of trials by the novelty filter and in 26.0 (18-35)% by a random choice of k agents.
median ms: think per agent 149.3, novelty select 0.5, summarizer batch of 8 prompts 750.6 (none 735.9, text_words 734.6); text prefix 9 tokens, vector 16384 bytes
### olmo2 -> qwen8b  (big, NVIDIA H100 80GB HBM3, bfloat16; map cosine 0.8846, 6 agents, k in [2, 3], 100 trials per k)
**olmo2 -> qwen8b, all k** (n = 200 trials)
| arm | mean recall % | all concepts recovered % (95% CI) | rare concepts found % | dominant found % | precision % |
|---|---|---|---|---|---|
| none | 5.8 | 0.0 (0-2) | 7.5 | 3.5 | 15.0 |
| text_words | 91.2 | 78.0 (72-83) | 89.2 | 95.0 | 92.2 |
| latent_one | 45.4 | 3.5 (2-7) | 19.8 | 78.0 | 53.0 |
| latent_all | 48.1 | 6.5 (4-11) | 14.8 | 94.0 | 52.8 |
| latent_random | 52.2 | 9.5 (6-14) | 28.7 | 84.0 | 55.2 |
| latent_novelty | 60.0 | 19.0 (14-25) | 59.8 | 60.0 | 61.4 |
| latent_novelty_x | 55.1 | 17.0 (12-23) | 51.7 | 60.0 | 63.1 |
| direct | 72.8 | 37.0 (31-44) | 73.0 | 73.5 | 74.7 |
| null | 11.4 | 0.5 (0-3) | 12.2 | 9.5 | 13.9 |
| contrast (recall) | first wins | second wins | exact p |
|---|---|---|---|
| latent_novelty vs latent_all | 74 | 19 | 7.72e-09 |
| latent_novelty vs latent_random | 52 | 16 | 1.41e-05 |
| latent_novelty vs latent_one | 80 | 9 | 2.31e-15 |
| latent_all vs latent_one | 27 | 12 | 0.0237 |
| latent_novelty_x vs latent_novelty | 24 | 47 | 0.00855 |
| latent_novelty vs none | 183 | 0 | 1.63e-55 |
| latent_novelty vs null | 164 | 5 | 2.98e-42 |
| text_words vs latent_novelty | 136 | 6 | 3.84e-33 |
| direct vs latent_novelty | 77 | 18 | 7.25e-10 |
One agent from every true concept was picked in 99.5 (97-100)% of trials by the novelty filter and in 32.0 (26-39)% by a random choice of k agents.
**olmo2 -> qwen8b, k = 2** (n = 100 trials)
| arm | mean recall % | all concepts recovered % (95% CI) | rare concepts found % | dominant found % | precision % |
|---|---|---|---|---|---|
| none | 4.5 | 0.0 (0-4) | 7.0 | 2.0 | 9.0 |
| text_words | 96.5 | 93.0 (86-97) | 96.0 | 97.0 | 97.0 |
| latent_one | 52.5 | 6.0 (3-12) | 15.0 | 90.0 | 58.5 |
| latent_all | 54.5 | 12.0 (7-20) | 13.0 | 96.0 | 58.5 |
| latent_random | 57.0 | 15.0 (9-23) | 27.0 | 87.0 | 59.5 |
| latent_novelty | 66.0 | 33.0 (25-43) | 65.0 | 67.0 | 67.5 |
| latent_novelty_x | 64.5 | 32.0 (24-42) | 61.0 | 68.0 | 68.0 |
| direct | 78.5 | 57.0 (47-66) | 81.0 | 76.0 | 79.5 |
| null | 9.5 | 1.0 (0-5) | 9.0 | 10.0 | 10.5 |
| contrast (recall) | first wins | second wins | exact p |
|---|---|---|---|
| latent_novelty vs latent_all | 30 | 7 | 0.000191 |
| latent_novelty vs latent_random | 24 | 6 | 0.00143 |
| latent_novelty vs latent_one | 31 | 4 | 3.47e-06 |
| latent_all vs latent_one | 9 | 5 | 0.424 |
| latent_novelty_x vs latent_novelty | 8 | 11 | 0.648 |
| latent_novelty vs none | 92 | 0 | 4.04e-28 |
| latent_novelty vs null | 85 | 1 | 2.25e-24 |
| text_words vs latent_novelty | 61 | 1 | 2.73e-17 |
| direct vs latent_novelty | 35 | 10 | 0.000247 |
One agent from every true concept was picked in 100.0 (96-100)% of trials by the novelty filter and in 38.0 (29-48)% by a random choice of k agents.
**olmo2 -> qwen8b, k = 3** (n = 100 trials)
| arm | mean recall % | all concepts recovered % (95% CI) | rare concepts found % | dominant found % | precision % |
|---|---|---|---|---|---|
| none | 7.0 | 0.0 (0-4) | 8.0 | 5.0 | 21.0 |
| text_words | 86.0 | 63.0 (53-72) | 82.5 | 93.0 | 87.3 |
| latent_one | 38.3 | 1.0 (0-5) | 24.5 | 66.0 | 47.5 |
| latent_all | 41.7 | 1.0 (0-5) | 16.5 | 92.0 | 47.2 |
| latent_random | 47.3 | 4.0 (2-10) | 30.5 | 81.0 | 51.0 |
| latent_novelty | 54.0 | 5.0 (2-11) | 54.5 | 53.0 | 55.3 |
| latent_novelty_x | 45.7 | 2.0 (1-7) | 42.5 | 52.0 | 58.2 |
| direct | 67.0 | 17.0 (11-26) | 65.0 | 71.0 | 69.8 |
| null | 13.3 | 0.0 (0-4) | 15.5 | 9.0 | 17.3 |
| contrast (recall) | first wins | second wins | exact p |
|---|---|---|---|
| latent_novelty vs latent_all | 44 | 12 | 2.09e-05 |
| latent_novelty vs latent_random | 28 | 10 | 0.0051 |
| latent_novelty vs latent_one | 49 | 5 | 3.89e-10 |
| latent_all vs latent_one | 18 | 7 | 0.0433 |
| latent_novelty_x vs latent_novelty | 16 | 36 | 0.00779 |
| latent_novelty vs none | 91 | 0 | 8.08e-28 |
| latent_novelty vs null | 79 | 4 | 4e-19 |
| text_words vs latent_novelty | 75 | 5 | 4.25e-17 |
| direct vs latent_novelty | 42 | 8 | 1.16e-06 |
One agent from every true concept was picked in 99.0 (95-100)% of trials by the novelty filter and in 26.0 (18-35)% by a random choice of k agents.
median ms: think per agent 137.7, novelty select 0.5, summarizer batch of 8 prompts 943.1 (none 928.5, text_words 923.4); text prefix 9 tokens, vector 16384 bytes
**big pair, both directions pooled** (n = 400 trials)
| arm | mean recall % | all concepts recovered % (95% CI) | rare concepts found % | dominant found % | precision % |
|---|---|---|---|---|---|
| none | 7.8 | 0.2 (0-1) | 9.4 | 5.8 | 13.8 |
| text_words | 85.9 | 66.0 (61-70) | 82.1 | 92.0 | 86.6 |
| latent_one | 44.5 | 2.8 (2-5) | 18.2 | 78.2 | 63.2 |
| latent_all | 46.7 | 5.5 (4-8) | 11.5 | 95.5 | 61.0 |
| latent_random | 49.6 | 8.0 (6-11) | 24.1 | 84.8 | 61.6 |
| latent_novelty | 55.7 | 17.0 (14-21) | 54.2 | 57.2 | 63.0 |
| latent_novelty_x | 43.2 | 12.5 (10-16) | 38.9 | 48.2 | 57.0 |
| direct | 67.0 | 32.0 (28-37) | 67.0 | 67.8 | 71.5 |
| null | 9.0 | 0.5 (0-2) | 10.4 | 6.5 | 11.7 |
| contrast (recall) | first wins | second wins | exact p |
|---|---|---|---|
| latent_novelty vs latent_all | 127 | 48 | 1.99e-09 |
| latent_novelty vs latent_random | 101 | 47 | 1.07e-05 |
| latent_novelty vs latent_one | 140 | 34 | 1.77e-16 |
| latent_all vs latent_one | 45 | 21 | 0.00427 |
| latent_novelty_x vs latent_novelty | 36 | 138 | 2.8e-15 |
| latent_novelty vs none | 336 | 6 | 4.83e-91 |
| latent_novelty vs null | 327 | 8 | 1.06e-85 |
| text_words vs latent_novelty | 257 | 20 | 1.29e-53 |
| direct vs latent_novelty | 155 | 53 | 8.62e-13 |
One agent from every true concept was picked in 99.0 (97-100)% of trials by the novelty filter and in 32.0 (28-37)% by a random choice of k agents.
wrote /Users/shaanp/Documents/GitHub/playbox/steer/results/telepathy/swarm/summary.json

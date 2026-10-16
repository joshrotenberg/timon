; Recent work on timon: sessions run in a timon directory, and the agent runs timon recorded.
; Usage: timon recent [SINCE], SINCE such as 2d (the default) or 6h
(local since (or (. arg 1) "2d"))
(local found (sessions.list {:project "timon" : since :limit 10}))
(print (.. "sessions since " since ": " (length found.sessions)))
(each [_ s (ipairs found.sessions)]
  (print (.. "  " (s.id:sub 1 8) "  " s.backend "  " (or s.title s.snippet ""))))
(local runs (agent.runs {: since :limit 10}))
(print (.. "agent runs since " since ": " (length runs)))
(each [_ r (ipairs runs)]
  (print (.. "  " r.id "  " r.status "  " (or r.prompt ""))))

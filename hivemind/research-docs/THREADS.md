# Life threads: routing, focus and private areas, in the kernel

A thread is an area of your life: work, home, health, a side project. The kernel
puts every task and note into one, and three things follow from that:

1. **Routing.** Your own rules decide first. `#work` anywhere in the text files it
   there; otherwise the thread whose keywords match most wins. Only when no rule
   decides (no match, or a tie) is the model asked, and the kernel accepts its
   answer only if it is the name of a real thread. Everything else goes to `inbox`.
2. **Focus.** `focus work 90` lets only work reminders interrupt for 90 minutes.
   Reminders from other threads still fire, but they are held and delivered as one
   digest when focus ends (by time or `focus off`).
3. **Private threads.** `thread private health` makes the kernel leave every item in
   that thread out of every model call: app calls (`ask`, note tagging) and agent
   `ai:` calls alike. The app does not have to remember to do this; the context
   builder in the kernel does it.

Every routing decision, every held reminder and every private hold-back is a
provenance event, so `why tasks t3_thread` tells you why an item went where it did.

## Commands
```
thread add work meeting email acme standup      # name + keywords (re-add replaces keywords)
thread add home milk rent laundry groceries
thread add health doctor meds gym
thread private health                          # thread public health to undo
thread                                         # threads, counts, focus, model routing
thread show work                               # tasks and notes in one thread
thread model off                               # rules only; unmatched items stay in inbox
route t3 home                                  # move an item by hand
focus work 90 | focus work | focus | focus off
why tasks t1_thread                            # e.g.  -> work (keyword: acme)
```
`task` and `note list` show each item's thread; the desktop Tasks window (F7) shows
it too, and shows the focus at the top.

## Untrusted input
A mesh peer can write values, but:
* thread definitions, focus and the model-routing switch written by a peer are
  ignored (a peer cannot make a private thread public);
* a thread label written by a peer is ignored for an item you wrote; the rules on
  that item's own text decide. A peer cannot move your private note into a public
  thread to get it sent to the model.

## Limits (honest)
* A private thread only protects items the kernel knows are in it. If no rule
  matches an item and model routing is on, the item is shown to the model **to be
  routed**. Give private threads good keywords, use `#health`, or `thread model off`.
* Keyword matching is whole words plus a plain `s` plural. No stemming, no synonyms.
* Focus holds reminders only. Model answers and other notifications still show.
* Held reminder keys and focus are saved to disk; a reminder that fires while the
  machine is off fires on the next boot.

"""Runtime configuration optimizer (user space).

space.py   parameter registry, encodings, and the validator for hard constraints
search.py  random search, grid search (only where feasible), Gaussian-process
           Bayesian optimization with expected improvement
Every search records evaluations, invalid proposals, best-so-far curve and cost.
"""

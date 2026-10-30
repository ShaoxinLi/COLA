# Architecture

Favor a three-layer architecture that makes responsibilities easy to locate and changes easier to reason about. Keep core rules separate from workflow coordination and external mechanisms, so each can evolve without forcing unrelated changes elsewhere.

- **Domain (`src/domain`)** expresses the system’s core concepts, rules, and invariants. It defines their meaning and valid behavior independently of how the system is accessed, executed, or persisted.
- **Application (`src/app`)** implements use cases by coordinating domain behavior and the capabilities those use cases require. It owns workflow sequencing and interacts with external capabilities through abstract contracts.
- **Adapters (`src/adapters`)** connect the application and domain to concrete technologies and external systems. They translate between external representations and internal concepts, and implement the mechanisms needed for input, output, and integration.

Prefer dependencies that point inward. Keep domain independent of application and adapters. Let application depend on domain and abstract contracts, with adapters implementing or invoking those contracts. Place each contract in the inner layer that needs it, and assemble concrete implementations at the outer boundary.

Use these boundaries as design defaults. Choose abstractions that clarify responsibilities and support real needs. When another arrangement better serves clarity, correctness, or maintainability, explain the tradeoff and preserve the intent of the separation.

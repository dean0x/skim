# Examples

This document provides examples of how Skim transforms different programming languages.

## TypeScript/JavaScript

### Class with Async Method

**Input:**
```typescript
class UserService {
    async findUser(id: string): Promise<User> {
        const user = await db.users.findOne({ id });
        if (!user) throw new NotFoundError();
        return user;
    }
}
```

**Output (structure mode):**
```typescript
class UserService {
    async findUser(id: string): Promise<User> { /* ... */ }
}
```

### Interface and Type Definitions

**Input:**
```typescript
interface User {
    id: string;
    name: string;
    email: string;
}

type UserRole = 'admin' | 'user' | 'guest';

class UserManager {
    getUser(id: string): User | null {
        return this.users.find(u => u.id === id) || null;
    }
}
```

**Output (structure mode):**
```typescript
interface User {
    id: string;
    name: string;
    email: string;
}

type UserRole = 'admin' | 'user' | 'guest';

class UserManager {
    getUser(id: string): User | null { /* ... */ }
}
```

**Output (types mode):**
```typescript
interface User {
    id: string;
    name: string;
    email: string;
}

type UserRole = 'admin' | 'user' | 'guest';
```

## Python

### Function with Type Hints

**Input:**
```python
def process_data(items: List[Item]) -> Dict[str, Any]:
    """Process items and return statistics"""
    results = {}
    for item in items:
        results[item.id] = calculate_metrics(item)
    return results
```

**Output (structure mode):**
```python
def process_data(items: List[Item]) -> Dict[str, Any]: { /* ... */ }
```

### Class with Methods

**Input:**
```python
class DataProcessor:
    def __init__(self, config: Config):
        self.config = config
        self.cache = {}

    async def process(self, data: pd.DataFrame) -> ProcessedData:
        validated = self.validate(data)
        transformed = self.transform(validated)
        return self.finalize(transformed)

    def validate(self, data: pd.DataFrame) -> pd.DataFrame:
        if data.empty:
            raise ValueError("Empty dataframe")
        return data
```

**Output (structure mode):**
```python
class DataProcessor:
    def __init__(self, config: Config): { /* ... */ }

    async def process(self, data: pd.DataFrame) -> ProcessedData: { /* ... */ }

    def validate(self, data: pd.DataFrame) -> pd.DataFrame: { /* ... */ }
```

## Rust

### Impl Block

**Input:**
```rust
impl UserRepository {
    pub async fn create(&self, user: NewUser) -> Result<User> {
        let validated = self.validate(user)?;
        let id = Uuid::new_v4();
        self.db.insert(id, validated).await
    }

    fn validate(&self, user: NewUser) -> Result<NewUser> {
        if user.email.is_empty() {
            return Err(Error::Validation("Email required"));
        }
        Ok(user)
    }
}
```

**Output (structure mode):**
```rust
impl UserRepository {
    pub async fn create(&self, user: NewUser) -> Result<User> { /* ... */ }

    fn validate(&self, user: NewUser) -> Result<NewUser> { /* ... */ }
}
```

### Struct and Trait

**Input:**
```rust
pub struct User {
    pub id: Uuid,
    pub name: String,
    pub email: String,
}

pub trait Repository {
    async fn find_by_id(&self, id: Uuid) -> Result<User>;
    async fn save(&self, user: User) -> Result<()>;
}
```

**Output (structure mode):**
```rust
pub struct User {
    pub id: Uuid,
    pub name: String,
    pub email: String,
}

pub trait Repository {
    async fn find_by_id(&self, id: Uuid) -> Result<User>;
    async fn save(&self, user: User) -> Result<()>;
}
```

## Go

### Function and Struct

**Input:**
```go
type UserService struct {
    db *Database
}

func (s *UserService) FindUser(id string) (*User, error) {
    user, err := s.db.Query("SELECT * FROM users WHERE id = ?", id)
    if err != nil {
        return nil, err
    }
    return user, nil
}
```

**Output (structure mode):**
```go
type UserService struct {
    db *Database
}

func (s *UserService) FindUser(id string) (*User, error) { /* ... */ }
```

## Java

### Class with Methods

**Input:**
```java
public class UserService {
    private Database db;

    public User findUser(String id) throws NotFoundException {
        User user = db.query("SELECT * FROM users WHERE id = ?", id);
        if (user == null) {
            throw new NotFoundException("User not found");
        }
        return user;
    }

    public void updateUser(User user) throws ValidationException {
        validate(user);
        db.update(user);
    }
}
```

**Output (structure mode):**
```java
public class UserService {
    private Database db;

    public User findUser(String id) throws NotFoundException { /* ... */ }

    public void updateUser(User user) throws ValidationException { /* ... */ }
}
```

## Markdown

### Header Extraction

**Input:**
```markdown
# Project Documentation

This is the introduction to our project.

## Getting Started

Follow these steps to get started.

### Prerequisites

You'll need Node.js installed.

#### Installation

Run npm install.

##### Details

More specific details here.
```

**Output (structure mode - H1-H3 only):**
```markdown
# Project Documentation
## Getting Started
### Prerequisites
```

**Output (signatures/types mode - H1-H6 all headers):**
```markdown
# Project Documentation
## Getting Started
### Prerequisites
#### Installation
##### Details
```

## Complex Examples

### TypeScript: Full Application Structure

**Input:**
```typescript
import { Request, Response } from 'express';
import { UserService } from './services';
import { ValidationError } from './errors';

export interface CreateUserDTO {
    name: string;
    email: string;
}

export class UserController {
    constructor(private userService: UserService) {}

    async createUser(req: Request, res: Response): Promise<void> {
        try {
            const dto: CreateUserDTO = req.body;
            const user = await this.userService.create(dto);
            res.status(201).json(user);
        } catch (error) {
            if (error instanceof ValidationError) {
                res.status(400).json({ error: error.message });
            } else {
                res.status(500).json({ error: 'Internal server error' });
            }
        }
    }
}
```

**Output (structure mode):**
```typescript
import { Request, Response } from 'express';
import { UserService } from './services';
import { ValidationError } from './errors';

export interface CreateUserDTO {
    name: string;
    email: string;
}

export class UserController {
    constructor(private userService: UserService) { /* ... */ }

    async createUser(req: Request, res: Response): Promise<void> { /* ... */ }
}
```

**Output (signatures mode):**
```typescript
constructor(private userService: UserService)
async createUser(req: Request, res: Response): Promise<void>
```

**Output (types mode):**
```typescript
export interface CreateUserDTO {
    name: string;
    email: string;
}
```

### Python: Data Processing Pipeline

**Input:**
```python
from typing import List, Dict, Any
import pandas as pd

class DataPipeline:
    """Pipeline for processing data"""

    def __init__(self, config: Dict[str, Any]):
        self.config = config
        self.transformers: List[Transformer] = []

    def add_transformer(self, transformer: Transformer) -> None:
        self.transformers.append(transformer)

    async def process(self, data: pd.DataFrame) -> pd.DataFrame:
        result = data.copy()
        for transformer in self.transformers:
            result = await transformer.transform(result)
        return result
```

**Output (structure mode):**
```python
from typing import List, Dict, Any
import pandas as pd

class DataPipeline:
    """Pipeline for processing data"""

    def __init__(self, config: Dict[str, Any]): { /* ... */ }

    def add_transformer(self, transformer: Transformer) -> None: { /* ... */ }

    async def process(self, data: pd.DataFrame) -> pd.DataFrame: { /* ... */ }
```

## Multi-File Example

When processing multiple files, Skim automatically detects each language:

```bash
$ tree src/
src/
├── api.ts
├── models.py
└── utils.rs

$ skim src/
// === src/api.ts ===
export class ApiClient { /* ... */ }

// === src/models.py ===
class User: { /* ... */ }

// === src/utils.rs ===
pub fn format_date() -> String { /* ... */ }
```

## Real-World Example

Processing the Chorus project (80 TypeScript files):

```bash
$ skim /workspace/chorus/src/ --show-stats

[skim] 63,198 tokens → 25,119 tokens (60.3% reduction) across 80 file(s)
```

See [Performance](./performance.md) for detailed benchmarks.

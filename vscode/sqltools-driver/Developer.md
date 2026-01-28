## Developer

### Prerequisites

- Typescript 4.9+
- Nodejs 20+
- Visual Studio Code

### Building

In the project directory run the following commands

```
npm install
npm run compile
```

### Run tests

```
# Run all tests
npm test

# Run only unit tests
npm run test:unit

# Run only integration tests
npm run test:integration
```

### Formatting and linting 

Run the following commands before submitting a PR (these are automatically verified):

```
npm run format
npm run lint:fix
```

### Third party notices

The THIRD-PARTY-LICENSES.json file lists the packages used by this extension.
Regenerate it whenever dependencies or their versions change in `package.json`:

```
npm run licenses:generate
```

# @james/api

Types for [James](https://github.com/Hudint/James) plugins. Install it as a dev dependency for type checking and
editor support:

```sh
# from a release (the version of your James; commit package-lock.json, it pins the file's hash)
npm install --save-dev @james/api@https://github.com/Hudint/James/releases/download/v0.5.2/james-api-0.5.2.tgz
# or from a checkout
npm install --save-dev <path to James>/packages/api
```

The plugin guide is [docs/plugins/README.md](https://github.com/Hudint/James/blob/main/docs/plugins/README.md);
the API reference is the [plugin API site](https://hudint.github.io/James/), generated from
`index.d.ts`. MIT licensed, so plugins may use any license.

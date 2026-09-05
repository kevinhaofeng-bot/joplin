const baseConfig = require('../../jest.config.base.js');

module.exports = {
	...baseConfig,
	testEnvironment: 'node',
	testMatch: ['**/*.test.ts'],
	transform: {
		'^.+\\.ts$': ['ts-jest', {
			tsconfig: 'tsconfig.json',
		}],
	},
};

#include <cstdio>
#include <cstdlib>
#include <cuda_runtime.h>

#define ELEMENTS 256
#define THREADS 32
#define BLOCKS 2

__global__ void reduce_sum(const int *values, int *partial)
{
    int index = blockIdx.x * blockDim.x + threadIdx.x;
    if (index < ELEMENTS) {
        atomicAdd(partial, values[index]);
    }
}

__global__ void reduce_max(const int *values, int *partial)
{
    int index = blockIdx.x * blockDim.x + threadIdx.x;
    if (index < ELEMENTS) {
        atomicMax(partial, values[index]);
    }
}

int main(void)
{
    int host_values[ELEMENTS];
    for (int index = 0; index < ELEMENTS; ++index) {
        host_values[index] = index;
    }

    int *device_values = NULL;
    int *device_sum = NULL;
    int *device_max = NULL;
    if (cudaMalloc(&device_values, sizeof(host_values)) != cudaSuccess
        || cudaMalloc(&device_sum, sizeof(int)) != cudaSuccess
        || cudaMalloc(&device_max, sizeof(int)) != cudaSuccess) {
        fprintf(stderr, "cuda allocation failed\n");
        return 1;
    }
    if (cudaMemcpy(device_values, host_values, sizeof(host_values), cudaMemcpyHostToDevice) != cudaSuccess
        || cudaMemset(device_sum, 0, sizeof(int)) != cudaSuccess
        || cudaMemset(device_max, 0, sizeof(int)) != cudaSuccess) {
        fprintf(stderr, "cuda transfer failed\n");
        return 1;
    }

    reduce_sum<<<BLOCKS, THREADS>>>(device_values, device_sum);
    reduce_max<<<BLOCKS, THREADS>>>(device_values, device_max);
    if (cudaDeviceSynchronize() != cudaSuccess) {
        fprintf(stderr, "kernel launch failed\n");
        return 1;
    }

    int total = 0;
    int largest = 0;
    if (cudaMemcpy(&total, device_sum, sizeof(int), cudaMemcpyDeviceToHost) != cudaSuccess
        || cudaMemcpy(&largest, device_max, sizeof(int), cudaMemcpyDeviceToHost) != cudaSuccess) {
        fprintf(stderr, "cuda readback failed\n");
        return 1;
    }

    cudaFree(device_values);
    cudaFree(device_sum);
    cudaFree(device_max);

    printf("N=%d\n", ELEMENTS);
    printf("sum=%d\n", total);
    printf("max=%d\n", largest);
    return 0;
}

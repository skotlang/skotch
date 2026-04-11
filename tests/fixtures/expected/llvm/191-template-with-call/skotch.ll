; ModuleID = 'InputKt'
source_filename = "InputKt.kt"

@.str.0 = private unnamed_addr constant [8 x i8] c"double(\00", align 1
@.str.1 = private unnamed_addr constant [5 x i8] c") = \00", align 1
@.str.2 = private unnamed_addr constant [4 x i8] c" * \00", align 1
@.str.3 = private unnamed_addr constant [4 x i8] c" = \00", align 1
@.fmt.int_println = private unnamed_addr constant [4 x i8] c"%d\0A\00", align 1
@.fmt.concat.0 = private unnamed_addr constant [17 x i8] c"double(%d) = %d\0A\00", align 1
@.fmt.concat.1 = private unnamed_addr constant [14 x i8] c"%d * %d = %d\0A\00", align 1

declare i32 @printf(ptr, ...)

define i32 @InputKt_double(i32 %arg0) {
entry:
  %t0 = add i32 0, 2
  %t1 = mul i32 %arg0, %t0
  ret i32 %t1
}

define i32 @main() {
entry:
  %t0 = add i32 0, 5
  %t1 = call i32 @InputKt_double(i32 %t0)
  call i32 (ptr, ...) @printf(ptr @.fmt.concat.0, i32 %t0, i32 %t1)
  %t3 = mul i32 %t0, %t0
  call i32 (ptr, ...) @printf(ptr @.fmt.concat.1, i32 %t0, i32 %t0, i32 %t3)
  ret i32 0
}

